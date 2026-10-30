//! Loading, atomic saving and file watching for `config.json` (spec §14.5).
//!
//! Responsibility: everything that touches the settings file on disk. A
//! crash at any point must never leave a half-written `config.json`, and a
//! hand edit that fails validation must never replace a working config.
//! Not responsible for: the semantics of settings (see `schema.rs`,
//! `validate.rs`) or distributing changes to running components (see
//! `app.rs`).
//!
//! Implements FR-MOD-2, FR-MOD-3, NFR-5.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify_debouncer_mini::notify::RecursiveMode;
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use serde_json::Value;

use super::migrate::migrate;
use super::schema::Config;
use super::validate::{ValidateOptions, ValidationIssue, errors_of, validate};
use crate::error::{CoreError, CoreResult};

/// Quiet period before a file change is reported, so editors that write in
/// several steps trigger one reload (spec §14.5).
const WATCH_DEBOUNCE: Duration = Duration::from_millis(500);

/// File names inside the config directory.
const CONFIG_FILE: &str = "config.json";
/// Temporary file written before the atomic rename.
const TMP_FILE: &str = "config.json.tmp";
/// Previous valid version, refreshed before every save.
const BACKUP_FILE: &str = "config.json.bak";
/// Generated JSON Schema for editors.
const SCHEMA_FILE: &str = "config.schema.json";

/// Locations of the config file and its companions.
#[derive(Debug, Clone)]
pub struct ConfigPaths {
    /// Directory holding all config files.
    pub dir: PathBuf,
}

impl ConfigPaths {
    /// Uses `dir` (normally `%APPDATA%\VRAM Gatekeeper`).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Path of `config.json`.
    pub fn config(&self) -> PathBuf {
        self.dir.join(CONFIG_FILE)
    }

    /// Path of the temporary save file.
    fn tmp(&self) -> PathBuf {
        self.dir.join(TMP_FILE)
    }

    /// Path of the backup copy.
    pub fn backup(&self) -> PathBuf {
        self.dir.join(BACKUP_FILE)
    }

    /// Path of the generated schema.
    pub fn schema(&self) -> PathBuf {
        self.dir.join(SCHEMA_FILE)
    }
}

/// Where the active config came from at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadSource {
    /// `config.json` parsed and validated.
    Primary,
    /// `config.json` was unusable; `config.json.bak` was used instead.
    Backup,
    /// No usable file; defaults are in effect.
    Defaults,
    /// First run: a default file was just created.
    Created,
}

/// Result of loading the config at startup.
#[derive(Debug)]
pub struct LoadOutcome {
    /// The config now in effect.
    pub config: Config,
    /// Problems with the file that was *rejected* (empty if it was accepted).
    pub issues: Vec<ValidationIssue>,
    /// Parse failure message for the rejected file, if any.
    pub parse_error: Option<String>,
    /// Which source supplied `config`.
    pub source: LoadSource,
    /// Previous schema version if the file was migrated in memory.
    pub migrated_from: Option<u32>,
}

/// Parses and migrates config text. Returns the config and the old schema
/// version if a migration ran.
///
/// # Errors
/// Returns a JSON error (with line/column when no migration was needed) or
/// [`CoreError::ConfigTooNew`].
pub fn parse_config(text: &str) -> CoreResult<(Config, Option<u32>)> {
    let value: Value = serde_json::from_str(text)?;
    let migrated = migrate(value)?;
    if migrated.from_version.is_none() {
        // Re-parse the original text so serde reports line and column.
        return Ok((serde_json::from_str(text)?, None));
    }
    Ok((serde_json::from_value(migrated.value)?, migrated.from_version))
}

/// Reads and validates one file without side effects.
fn try_read(path: &Path) -> Result<(Config, Option<u32>), (String, Vec<ValidationIssue>)> {
    let text = fs::read_to_string(path).map_err(|e| (format!("cannot read {}: {e}", path.display()), Vec::new()))?;
    let (config, migrated_from) = parse_config(&text).map_err(|e| (e.to_string(), Vec::new()))?;
    // File existence is not checked here: a model on an unplugged drive must
    // not make the whole config unusable. The GUI checks files on save.
    // Only errors reject a file; warnings and notes are shown but do not stop it applying.
    let issues = errors_of(&validate(&config, ValidateOptions { check_files: false }));
    if issues.is_empty() { Ok((config, migrated_from)) } else { Err(("configuration is invalid".to_owned(), issues)) }
}

/// Loads the config at startup, following spec §21: invalid file → last
/// good backup → defaults; missing file → create defaults.
///
/// # Errors
/// Only fails if the directory cannot be created or a fresh default file
/// cannot be written.
pub fn load_or_init(paths: &ConfigPaths) -> CoreResult<LoadOutcome> {
    fs::create_dir_all(&paths.dir).map_err(|e| CoreError::io(&paths.dir, e))?;

    if !paths.config().exists() {
        let config = Config::default();
        save(paths, &config)?;
        return Ok(LoadOutcome {
            config,
            issues: Vec::new(),
            parse_error: None,
            source: LoadSource::Created,
            migrated_from: None,
        });
    }

    match try_read(&paths.config()) {
        Ok((config, migrated_from)) => Ok(LoadOutcome {
            config,
            issues: Vec::new(),
            parse_error: None,
            source: LoadSource::Primary,
            migrated_from,
        }),
        Err((parse_error, issues)) => {
            // Keep the broken file untouched so the user can fix it (spec §14.5).
            let (config, source) = match try_read(&paths.backup()) {
                Ok((config, _)) => (config, LoadSource::Backup),
                Err(_) => (Config::default(), LoadSource::Defaults),
            };
            Ok(LoadOutcome { config, issues, parse_error: Some(parse_error), source, migrated_from: None })
        }
    }
}

/// Re-reads `config.json` after a hand edit. An invalid file is reported
/// and *not* applied.
///
/// # Errors
/// Returns the parse error message or the list of validation problems.
pub fn reload(paths: &ConfigPaths) -> Result<Config, (String, Vec<ValidationIssue>)> {
    try_read(&paths.config()).map(|(c, _)| c)
}

/// Atomically saves `config`: write a temp file, flush it to disk, copy the
/// current file to `.bak`, then rename the temp file over `config.json`.
///
/// # Errors
/// Returns [`CoreError::Io`] if any step fails; `config.json` is untouched
/// unless the final rename succeeded.
pub fn save(paths: &ConfigPaths, config: &Config) -> CoreResult<()> {
    fs::create_dir_all(&paths.dir).map_err(|e| CoreError::io(&paths.dir, e))?;
    let json = serde_json::to_string_pretty(config)?;

    let tmp = paths.tmp();
    {
        let mut file = fs::File::create(&tmp).map_err(|e| CoreError::io(&tmp, e))?;
        file.write_all(json.as_bytes()).map_err(|e| CoreError::io(&tmp, e))?;
        file.write_all(b"\n").map_err(|e| CoreError::io(&tmp, e))?;
        // Flush to disk before the rename so a power cut cannot leave an
        // empty file under the final name.
        file.sync_all().map_err(|e| CoreError::io(&tmp, e))?;
    }

    let current = paths.config();
    // Only back up a file that is itself valid (a missing one is not), so the
    // backup is always something we can fall back to.
    if try_read(&current).is_ok() {
        fs::copy(&current, paths.backup()).map_err(|e| CoreError::io(paths.backup(), e))?;
    }
    // On Windows `rename` replaces an existing destination (MoveFileExW with
    // MOVEFILE_REPLACE_EXISTING), which is atomic on NTFS.
    fs::rename(&tmp, &current).map_err(|e| CoreError::io(&current, e))
}

/// Writes `config.schema.json` next to the config (spec D7).
///
/// # Errors
/// Returns [`CoreError::Io`] if the file cannot be written.
pub fn write_schema(paths: &ConfigPaths) -> CoreResult<()> {
    let text = serde_json::to_string_pretty(&Config::json_schema())?;
    fs::write(paths.schema(), text).map_err(|e| CoreError::io(paths.schema(), e))
}

/// Keeps the file watcher alive; dropping it stops watching.
pub struct ConfigWatcher {
    /// Held only for its `Drop`.
    _debouncer: Debouncer<notify_debouncer_mini::notify::RecommendedWatcher>,
}

/// Watches `config.json` and calls `on_change` (from the watcher thread)
/// after each debounced change. The *directory* is watched because atomic
/// saves replace the file, which would orphan a watch on the file itself.
///
/// # Errors
/// Returns [`CoreError::Platform`] if the watcher cannot be created.
pub fn watch(paths: &ConfigPaths, on_change: impl Fn() + Send + 'static) -> CoreResult<ConfigWatcher> {
    let target = paths.config();
    let mut debouncer = new_debouncer(WATCH_DEBOUNCE, move |res: DebounceEventResult| {
        if let Ok(events) = res
            && events.iter().any(|e| e.path == target)
        {
            on_change();
        }
    })
    .map_err(|e| CoreError::Platform(format!("cannot create config watcher: {e}")))?;
    debouncer
        .watcher()
        .watch(&paths.dir, RecursiveMode::NonRecursive)
        .map_err(|e| CoreError::Platform(format!("cannot watch {}: {e}", paths.dir.display())))?;
    Ok(ConfigWatcher { _debouncer: debouncer })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, ConfigPaths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = ConfigPaths::new(dir.path());
        (dir, p)
    }

    #[test]
    fn first_run_creates_defaults() {
        let (_d, p) = paths();
        let out = load_or_init(&p).expect("load");
        assert_eq!(out.source, LoadSource::Created);
        assert!(p.config().exists());
        assert_eq!(out.config, Config::default());
    }

    #[test]
    fn save_then_load_round_trips_and_keeps_unknown_fields() {
        let (_d, p) = paths();
        fs::create_dir_all(&p.dir).expect("mkdir");
        fs::write(p.config(), r#"{ "future_setting": {"a": 1}, "server": {"port": 9000} }"#).expect("write");
        let out = load_or_init(&p).expect("load");
        assert_eq!(out.config.server.port, 9000);
        save(&p, &out.config).expect("save");
        let text = fs::read_to_string(p.config()).expect("read");
        assert!(text.contains("future_setting"), "unknown field preserved: {text}");
    }

    #[test]
    fn omitted_fields_take_defaults() {
        let (cfg, _) = parse_config("{}").expect("parse");
        assert_eq!(cfg.server.port, super::super::defaults::DEFAULT_PORT);
    }

    #[test]
    fn game_list_accepts_plain_strings() {
        let (cfg, _) = parse_config(r#"{ "games": { "executables": ["game.exe"] } }"#).expect("parse");
        assert_eq!(cfg.games.executables[0].name, "game.exe");
        assert!(cfg.games.executables[0].enabled);
    }

    #[test]
    fn invalid_file_falls_back_to_backup_and_is_left_untouched() {
        let (_d, p) = paths();
        let mut good = Config::default();
        good.server.port = 9001;
        save(&p, &good).expect("save 1");
        save(&p, &good).expect("save 2 creates a backup of the first");
        fs::write(p.config(), r#"{ "server": { "port": "oops" } }"#).expect("corrupt");
        let out = load_or_init(&p).expect("load");
        assert_eq!(out.source, LoadSource::Backup);
        assert_eq!(out.config.server.port, 9001);
        assert!(out.parse_error.is_some());
        assert!(fs::read_to_string(p.config()).expect("read").contains("oops"));
    }

    #[test]
    fn validation_failure_is_reported_with_paths() {
        let (_d, p) = paths();
        fs::create_dir_all(&p.dir).expect("mkdir");
        fs::write(p.config(), r#"{ "vram": { "pressure": { "threshold_percent": 0 } } }"#).expect("write");
        let out = load_or_init(&p).expect("load");
        assert_eq!(out.source, LoadSource::Defaults);
        assert_eq!(out.issues[0].path, "vram.pressure.threshold_percent");
    }

    #[test]
    fn warning_only_edit_is_applied() {
        let (_d, p) = paths();
        fs::create_dir_all(&p.dir).expect("mkdir");
        // A reserved key in llama_args is a warning: the launcher skips it.
        fs::write(
            p.config(),
            r#"{ "models": [ { "id": "a", "files": { "model": "a.gguf" }, "llama_args": { "port": 1 } } ] }"#,
        )
        .expect("write");
        let cfg = reload(&p).expect("a warning does not reject the file");
        assert_eq!(cfg.models[0].id, "a");
        assert_eq!(load_or_init(&p).expect("load").source, LoadSource::Primary);
    }

    #[test]
    fn files_draft_defaults_to_none_for_existing_files() {
        let (cfg, _) =
            parse_config(r#"{ "models": [ { "id": "a", "files": { "model": "a.gguf" } } ] }"#).expect("parse");
        assert_eq!(cfg.models[0].files.draft, None);
        assert_eq!(cfg.schema_version, 1, "the new field is additive");
    }

    #[test]
    fn config_with_legacy_draft_key_still_loads_and_saves() {
        let (_d, p) = paths();
        fs::create_dir_all(&p.dir).expect("mkdir");
        // Written by hand or through the Parameters tab before the draft file had its own field.
        fs::write(
            p.config(),
            r#"{ "models": [ { "id": "a", "files": { "model": "a.gguf" }, "llama_args": { "spec-draft-model": "d.gguf", "spec-type": "draft-simple" } } ] }"#,
        )
        .expect("write");
        let out = load_or_init(&p).expect("load");
        assert_eq!(out.source, LoadSource::Primary, "reserved key is a warning, not a rejection");
        save(&p, &out.config).expect("save");
        assert!(
            fs::read_to_string(p.config()).expect("read").contains("spec-draft-model"),
            "kept for Move to Draft File"
        );
    }

    #[test]
    fn reload_rejects_invalid_edit() {
        let (_d, p) = paths();
        fs::create_dir_all(&p.dir).expect("mkdir");
        fs::write(p.config(), "{ not json").expect("write");
        assert!(reload(&p).is_err());
    }

    #[test]
    fn schema_is_written() {
        let (_d, p) = paths();
        fs::create_dir_all(&p.dir).expect("mkdir");
        write_schema(&p).expect("schema");
        let text = fs::read_to_string(p.schema()).expect("read");
        assert!(text.contains("pressure"));
    }
}
