//! Cross-field validation of a parsed [`Config`] (spec §14.4).
//!
//! Responsibility: produce a list of `{ path, message }` problems that the
//! GUI maps onto form fields and the file watcher shows when a hand edit is
//! rejected. Serde has already caught type errors by this point.
//! Not responsible for: parsing (`store.rs`) or catalog-aware checks of
//! `llama_args` values (`params::argv` reports those as warnings).
//!
//! Implements FR-MOD-3.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;

use super::schema::{Config, ModelConfig};
use crate::params::overlay;

/// HTTP statuses allowed for gated requests (spec §14.3).
const ALLOWED_ERROR_STATUSES: [u16; 4] = [409, 423, 429, 503];

/// Longest allowed model id (spec §14.3).
const MAX_ID_LEN: usize = 64;

/// How serious a finding is (spec §14.4). Only errors block saving and reject
/// a hand edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The setting cannot work; saving is refused.
    Error,
    /// The setting is accepted but probably not what was meant.
    Warning,
    /// A note the user may want to read.
    Info,
}

/// One validation finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationIssue {
    /// JSON path of the offending field, e.g. `models[2].llama_args.ctx-size`.
    pub path: String,
    /// Plain-language explanation.
    pub message: String,
    /// Whether the finding blocks saving.
    pub severity: Severity,
}

impl ValidationIssue {
    /// Builds an error, the severity every rule had before severities existed.
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self { path: path.into(), message: message.into(), severity: Severity::Error }
    }

    /// Builds a warning.
    pub fn warning(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self { severity: Severity::Warning, ..Self::new(path, message) }
    }

    /// Builds an information note.
    pub fn info(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self { severity: Severity::Info, ..Self::new(path, message) }
    }
}

/// The findings that block saving (spec §14.4).
pub fn errors_of(issues: &[ValidationIssue]) -> Vec<ValidationIssue> {
    issues.iter().filter(|i| i.severity == Severity::Error).cloned().collect()
}

/// Options controlling checks that touch the file system.
#[derive(Debug, Clone, Copy)]
pub struct ValidateOptions {
    /// Verify that enabled models' files exist on disk.
    pub check_files: bool,
}

impl Default for ValidateOptions {
    fn default() -> Self {
        Self { check_files: true }
    }
}

/// Message for an id or alias that breaks the id rules.
const ID_RULE: &str = "must be 1-64 characters from a-z A-Z 0-9 . _ : -";

/// Message for a count that must not be zero.
const AT_LEAST_ONE: &str = "must be at least 1";

/// Validates `config`, returning every problem found (empty means valid).
pub fn validate(config: &Config, options: ValidateOptions) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    check_server(config, &mut issues);
    check_games(config, &mut issues);
    check_vram(config, &mut issues);
    check_models(config, options, &mut issues);
    issues
}

/// Server section rules.
fn check_server(config: &Config, issues: &mut Vec<ValidationIssue>) {
    let s = &config.server;
    // Port 0 is allowed: it asks the OS for any free port, which the tests use.
    // Implements FR-REQ-1 safety rule from spec §8.1: LAN needs an API key.
    if s.allow_lan && !s.api_key_set {
        issues.push(ValidationIssue::new(
            "server.allow_lan",
            "LAN access requires an API key; set one before enabling it",
        ));
    }
    // Binding a non-loopback address by hand is LAN access too.
    if !s.allow_lan && !is_loopback_host(&s.host) && !s.api_key_set {
        issues.push(ValidationIssue::new(
            "server.host",
            "a non-local address exposes the proxy to the network and requires an API key",
        ));
    }
    if s.max_request_body_mib == 0 {
        issues.push(ValidationIssue::new("server.max_request_body_mib", AT_LEAST_ONE));
    }
    if let Some(default) = &s.default_model
        && config.find_model(default).is_none()
    {
        issues.push(ValidationIssue::new(
            "server.default_model",
            format!("no enabled model with id or alias '{default}'"),
        ));
    }
    if !ALLOWED_ERROR_STATUSES.contains(&config.errors.status_code) {
        issues.push(ValidationIssue::new("errors.status_code", "must be one of 409, 423, 429 or 503"));
    }
}

/// Games section rules.
fn check_games(config: &Config, issues: &mut Vec<ValidationIssue>) {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (i, exe) in config.games.executables.iter().enumerate() {
        let path = format!("games.executables[{i}].name");
        let lower = exe.name.to_ascii_lowercase();
        if !lower.ends_with(".exe") || exe.name.contains(['/', '\\']) {
            issues.push(ValidationIssue::new(&path, "must be a bare file name ending in .exe"));
        }
        if let Some(first) = seen.insert(lower, i) {
            issues.push(ValidationIssue::new(&path, format!("duplicates games.executables[{first}]")));
        }
    }
    if config.games.scan_interval_seconds == 0 {
        issues.push(ValidationIssue::new("games.scan_interval_seconds", AT_LEAST_ONE));
    }
}

/// VRAM section rules.
fn check_vram(config: &Config, issues: &mut Vec<ValidationIssue>) {
    let p = &config.vram.pressure;
    if !(1..=100).contains(&p.threshold_percent) {
        issues.push(ValidationIssue::new("vram.pressure.threshold_percent", "must be between 1 and 100"));
    }
    let g = &config.vram.loop_guard;
    if g.max_unloads == 0 {
        issues.push(ValidationIssue::new("vram.loop_guard.max_unloads", AT_LEAST_ONE));
    }
    if g.window_minutes == 0 {
        issues.push(ValidationIssue::new("vram.loop_guard.window_minutes", AT_LEAST_ONE));
    }
    if config.downloads.max_concurrent == 0 {
        issues.push(ValidationIssue::new("downloads.max_concurrent", AT_LEAST_ONE));
    }
}

/// Model rules: ids, aliases, files, reserved parameters.
fn check_models(config: &Config, options: ValidateOptions, issues: &mut Vec<ValidationIssue>) {
    // name (lowercased) -> where it was first claimed
    let mut names: HashMap<String, String> = HashMap::new();

    for (i, m) in config.models.iter().enumerate() {
        let base = format!("models[{i}]");
        if !valid_id(&m.id) {
            issues.push(ValidationIssue::new(format!("{base}.id"), ID_RULE));
        }
        claim(&mut names, &m.id, format!("{base}.id"), issues);
        for (j, alias) in m.aliases.iter().enumerate() {
            let path = format!("{base}.aliases[{j}]");
            if !valid_id(alias) {
                issues.push(ValidationIssue::new(&path, ID_RULE));
            }
            claim(&mut names, alias, path, issues);
        }

        if m.enabled {
            check_model_files(m, &base, options, issues);
        }
        check_model_args(m, &base, issues);
        issues.extend(crate::speculative::findings(m, &base, None, options.check_files));

        if m.lifecycle.load_timeout_seconds == 0 {
            issues.push(ValidationIssue::new(format!("{base}.lifecycle.load_timeout_seconds"), AT_LEAST_ONE));
        }
    }
}

/// Records `name` as used, flagging a clash with an earlier id or alias.
fn claim(names: &mut HashMap<String, String>, name: &str, path: String, issues: &mut Vec<ValidationIssue>) {
    if name.is_empty() {
        return;
    }
    if let Some(first) = names.insert(name.to_ascii_lowercase(), path.clone()) {
        issues.push(ValidationIssue::new(path, format!("'{name}' is already used by {first}")));
    }
}

/// True if `host` names this machine only.
pub fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// True for ids matching `^[a-zA-Z0-9._:-]{1,64}$`.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// Model file rules.
fn check_model_files(m: &ModelConfig, base: &str, options: ValidateOptions, issues: &mut Vec<ValidationIssue>) {
    if m.files.model.trim().is_empty() {
        issues.push(ValidationIssue::new(format!("{base}.files.model"), "a model file is required"));
        return;
    }
    if options.check_files {
        if !Path::new(&m.files.model).is_file() {
            issues.push(ValidationIssue::new(format!("{base}.files.model"), "file does not exist"));
        }
        if let Some(mm) = &m.files.mmproj
            && !Path::new(mm).is_file()
        {
            issues.push(ValidationIssue::new(format!("{base}.files.mmproj"), "file does not exist"));
        }
    }
}

/// Rejects reserved parameters in `llama_args` and `extra_args`.
fn check_model_args(m: &ModelConfig, base: &str, issues: &mut Vec<ValidationIssue>) {
    // A reserved key in `llama_args` is skipped at launch and listed in the
    // preview (spec §14.4), so it must not make an older file unusable.
    for key in m.llama_args.keys() {
        if overlay::is_reserved(key) {
            issues.push(ValidationIssue::warning(
                format!("{base}.llama_args.{key}"),
                "this parameter is managed by Gatekeeper and is not passed to the engine",
            ));
        }
    }
    for (j, arg) in m.extra_args.iter().enumerate() {
        if arg.starts_with('-') && overlay::is_reserved(arg.split('=').next().unwrap_or(arg)) {
            issues.push(ValidationIssue::new(
                format!("{base}.extra_args[{j}]"),
                "this parameter is managed by Gatekeeper and cannot be set per model",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{GameExecutable, ModelConfig};

    fn model(id: &str) -> ModelConfig {
        ModelConfig {
            id: id.into(),
            files: crate::config::schema::ModelFiles { model: "x.gguf".into(), mmproj: None, draft: None },
            ..ModelConfig::default()
        }
    }

    fn issues(c: &Config) -> Vec<ValidationIssue> {
        validate(c, ValidateOptions { check_files: false })
    }

    fn paths(v: &[ValidationIssue]) -> Vec<&str> {
        v.iter().map(|i| i.path.as_str()).collect()
    }

    #[test]
    fn default_config_is_valid() {
        assert!(issues(&Config::default()).is_empty());
    }

    #[test]
    fn lan_without_key_is_rejected() {
        let mut c = Config::default();
        c.server.allow_lan = true;
        assert_eq!(paths(&issues(&c)), ["server.allow_lan"]);
        c.server.api_key_set = true;
        assert!(issues(&c).is_empty());
    }

    #[test]
    fn non_loopback_host_without_key_is_rejected() {
        let mut c = Config::default();
        c.server.host = "0.0.0.0".into();
        assert_eq!(paths(&issues(&c)), ["server.host"]);
        c.server.host = "localhost".into();
        assert!(issues(&c).is_empty());
    }

    #[test]
    fn duplicate_ids_and_aliases_are_rejected_case_insensitively() {
        let mut c = Config::default();
        let mut a = model("qwen");
        a.aliases = vec!["chat".into()];
        let mut b = model("QWEN");
        b.aliases = vec!["Chat".into()];
        c.models = vec![a, b];
        let p = issues(&c);
        assert_eq!(paths(&p), ["models[1].id", "models[1].aliases[0]"]);
    }

    #[test]
    fn invalid_id_is_rejected() {
        let c = Config { models: vec![model("has space")], ..Config::default() };
        assert_eq!(paths(&issues(&c)), ["models[0].id"]);
    }

    #[test]
    fn reserved_args_are_rejected_in_both_places() {
        let mut c = Config::default();
        let mut m = model("a");
        m.llama_args.insert("port".into(), 1.into());
        m.extra_args = vec!["--api-key=abc".into()];
        c.models = vec![m];
        let found = issues(&c);
        assert_eq!(paths(&found), ["models[0].llama_args.port", "models[0].extra_args[0]"]);
        // A key in llama_args is skipped at launch, a flag in extra_args is passed verbatim.
        assert_eq!(found[0].severity, Severity::Warning);
        assert_eq!(found[1].severity, Severity::Error);
        assert_eq!(errors_of(&found).len(), 1);
    }

    #[test]
    fn constructors_set_the_severity() {
        assert_eq!(ValidationIssue::new("a", "b").severity, Severity::Error);
        assert_eq!(ValidationIssue::warning("a", "b").severity, Severity::Warning);
        assert_eq!(ValidationIssue::info("a", "b").severity, Severity::Info);
    }

    #[test]
    fn game_names_must_be_bare_exe_and_unique() {
        let mut c = Config::default();
        let exe = |n: &str| GameExecutable { name: n.into(), path: None, enabled: true };
        c.games.executables = vec![exe("a.exe"), exe("A.EXE"), exe("dir\\b.exe"), exe("noext")];
        assert_eq!(
            paths(&issues(&c)),
            ["games.executables[1].name", "games.executables[2].name", "games.executables[3].name"]
        );
    }

    #[test]
    fn out_of_range_values_are_rejected() {
        let mut c = Config::default();
        c.vram.pressure.threshold_percent = 0;
        c.errors.status_code = 200;
        assert_eq!(paths(&issues(&c)), ["errors.status_code", "vram.pressure.threshold_percent"]);
    }

    #[test]
    fn missing_draft_file_is_reported_when_checking_files() {
        let mut m = model("a");
        m.files.draft = Some("definitely-not-here-draft.gguf".into());
        let c = Config { models: vec![m], ..Config::default() };
        let p = validate(&c, ValidateOptions { check_files: true });
        assert!(paths(&p).contains(&"models[0].files.draft"), "{p:?}");
        assert!(errors_of(&issues(&c)).is_empty(), "no file checks when check_files is off");
    }

    #[test]
    fn missing_model_file_is_reported_when_checking_files() {
        let c = Config { models: vec![model("a")], ..Config::default() };
        let p = validate(&c, ValidateOptions { check_files: true });
        assert_eq!(paths(&p), ["models[0].files.model"]);
    }
}
