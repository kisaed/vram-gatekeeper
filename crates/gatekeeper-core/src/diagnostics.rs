//! Diagnostics bundle and model test load (spec §10.5 item 8, §20).
//!
//! Responsibility: package what a maintainer needs to understand a problem
//! (logs, the redacted config, learned state, adapter list, engine and Windows
//! versions) into one zip the user can attach, and describe the outcome of a
//! test load.
//! Not responsible for: sending anything anywhere. The bundle is created only
//! when the user asks, and secrets and personal paths are removed first.
//!
//! Implements FR-APP-4.

use std::io::Write;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::error::{CoreError, CoreResult};

/// Lines kept from each model log.
pub const MODEL_LOG_LINES: usize = 1000;

/// Marker that replaces secrets.
const REDACTED: &str = "[redacted]";

/// Names whose string values are secrets, whatever their depth.
const SECRET_WORDS: [&str; 4] = ["key", "token", "secret", "password"];

/// What a test load found (spec §10.5 item 8).
#[derive(Debug, Clone, Serialize)]
pub struct TestLoadReport {
    /// The model started and answered its health check.
    pub ok: bool,
    /// Seconds from request to ready.
    pub load_seconds: f64,
    /// GPU layers `(offloaded, total)`, if the engine reported them.
    pub gpu_layers: Option<(u32, u32)>,
    /// VRAM the model occupies, in MiB.
    pub footprint_mib: Option<u64>,
    /// True when the footprint is an estimate rather than a measurement.
    pub footprint_estimated: bool,
    /// Overflow into system memory, described, if detected.
    pub overflow: Option<String>,
    /// A sentence for the user: what happened, or why it failed.
    pub message: String,
    /// For each speculative type configured for the model, whether the
    /// engine's log confirmed it started (spec §10.6). Empty when none is set.
    pub speculative: Vec<crate::speculative::SpecConfirmation>,
}

impl TestLoadReport {
    /// A failed test load with nothing measured.
    pub fn failed(load_seconds: f64, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            load_seconds,
            gpu_layers: None,
            footprint_mib: None,
            footprint_estimated: false,
            overflow: None,
            message: message.into(),
            speculative: Vec::new(),
        }
    }
}

/// Replaces personal folder names in `text` with `%USERPROFILE%`, for both
/// slash styles and the JSON-escaped form.
pub fn redact_paths(text: &str, home: &str) -> String {
    let home = home.trim_end_matches(['\\', '/']);
    if home.is_empty() {
        return text.to_owned();
    }
    let forward = home.replace('\\', "/");
    let escaped = home.replace('\\', "\\\\");
    let mut out = text.to_owned();
    // Longest spelling first so the escaped form is not half-replaced.
    for spelling in [escaped.as_str(), home, forward.as_str()] {
        out = replace_ignore_ascii_case(&out, spelling, "%USERPROFILE%");
    }
    out
}

/// Case-insensitive replacement (Windows paths ignore case).
fn replace_ignore_ascii_case(text: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return text.to_owned();
    }
    let (lower_text, lower_needle) = (text.to_ascii_lowercase(), needle.to_ascii_lowercase());
    let mut out = String::with_capacity(text.len());
    let mut from = 0;
    while let Some(at) = lower_text[from..].find(&lower_needle) {
        out.push_str(&text[from..from + at]);
        out.push_str(with);
        from += at + needle.len();
    }
    out.push_str(&text[from..]);
    out
}

/// Blanks every string value whose key looks like a secret.
///
/// `api_key_set` and similar booleans are kept: they say whether a key exists
/// without revealing it.
pub fn redact_secrets(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (name, v) in map.iter_mut() {
                let lower = name.to_ascii_lowercase();
                if v.is_string() && SECRET_WORDS.iter().any(|w| lower.contains(w)) {
                    *v = Value::String(REDACTED.into());
                } else {
                    redact_secrets(v);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_secrets),
        _ => {}
    }
}

/// Pretty JSON of `config` with secrets blanked and personal paths removed.
///
/// # Errors
/// Returns an error if the value cannot be serialised.
pub fn redacted_json<T: Serialize>(config: &T, home: &str) -> CoreResult<String> {
    let mut value = serde_json::to_value(config).map_err(|e| CoreError::Other(format!("config to JSON: {e}")))?;
    redact_secrets(&mut value);
    let text = serde_json::to_string_pretty(&value).map_err(|e| CoreError::Other(format!("config to JSON: {e}")))?;
    Ok(redact_paths(&text, home))
}

/// One entry to put in the zip.
pub struct Entry {
    /// Path inside the zip.
    pub name: String,
    /// File content.
    pub data: Vec<u8>,
}

impl Entry {
    /// A text entry.
    pub fn text(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self { name: name.into(), data: text.into().into_bytes() }
    }
}

/// Writes `entries` to a zip at `dest`.
///
/// # Errors
/// Returns an error if the file cannot be created or written.
pub fn write_zip(dest: &Path, entries: &[Entry]) -> CoreResult<()> {
    let io = |e: &dyn std::fmt::Display| CoreError::Other(format!("writing {}: {e}", dest.display()));
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| io(&e))?;
    }
    let file = std::fs::File::create(dest).map_err(|e| io(&e))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for entry in entries {
        zip.start_file(&entry.name, options).map_err(|e| io(&e))?;
        zip.write_all(&entry.data).map_err(|e| io(&e))?;
    }
    zip.finish().map_err(|e| io(&e))?;
    Ok(())
}

/// The last `n` lines of text.
pub fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Windows version text, e.g. `Microsoft Windows [Version 10.0.26300.1]`.
#[cfg(windows)]
pub fn windows_version() -> String {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("cmd")
        .args(["/C", "ver"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|_| "unknown".into())
}

/// Operating system name on other platforms (the app targets Windows).
#[cfg(not(windows))]
pub fn windows_version() -> String {
    std::env::consts::OS.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HOME: &str = r"C:\Users\alice";

    #[test]
    fn personal_paths_are_removed_in_every_spelling() {
        let text =
            r#"{"a":"C:\\Users\\alice\\models\\x.gguf","b":"c:/users/Alice/models/y.gguf","c":"C:\Users\ALICE\z"}"#;
        let out = redact_paths(text, HOME);
        assert!(!out.to_ascii_lowercase().contains("alice"), "{out}");
        assert_eq!(out.matches("%USERPROFILE%").count(), 3);
        assert!(out.contains("models"), "the rest of the path stays useful");
    }

    #[test]
    fn empty_home_changes_nothing() {
        assert_eq!(redact_paths("abc", ""), "abc");
    }

    #[test]
    fn secrets_are_blanked_at_any_depth_but_flags_survive() {
        let mut v = json!({
            "server": { "api_key_set": true, "api_key": "sk-abc" },
            "huggingface": { "token": "hf_123", "note": "x" },
            "models": [ { "env": { "MY_SECRET": "s3", "PATH": "p" } } ],
        });
        redact_secrets(&mut v);
        assert_eq!(v["server"]["api_key_set"], json!(true));
        assert_eq!(v["server"]["api_key"], json!(REDACTED));
        assert_eq!(v["huggingface"]["token"], json!(REDACTED));
        assert_eq!(v["huggingface"]["note"], json!("x"));
        assert_eq!(v["models"][0]["env"]["MY_SECRET"], json!(REDACTED));
        assert_eq!(v["models"][0]["env"]["PATH"], json!("p"));
    }

    #[test]
    fn bundle_is_a_readable_zip_with_the_given_entries() {
        let dir = tempfile::tempdir().expect("dir");
        let dest = dir.path().join("out").join("bundle.zip");
        write_zip(&dest, &[Entry::text("a.txt", "hello"), Entry::text("logs/b.log", "line1\nline2")]).expect("zip");
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&dest).expect("open")).expect("archive");
        assert_eq!(archive.len(), 2);
        let mut text = String::new();
        std::io::Read::read_to_string(&mut archive.by_name("logs/b.log").expect("entry"), &mut text).expect("read");
        assert_eq!(text, "line1\nline2");
    }

    #[test]
    fn last_lines_keeps_the_tail() {
        assert_eq!(last_lines("a\nb\nc\nd", 2), "c\nd");
        assert_eq!(last_lines("a", 5), "a");
        assert_eq!(last_lines("", 5), "");
    }
}
