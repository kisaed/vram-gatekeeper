//! `state.json`: learned data that is not a setting (spec D7, §14.1).
//!
//! Responsibility: remember measured VRAM footprints between runs so
//! make-room and loop-guard decisions are accurate from the first request.
//! Safe to delete: everything here is re-learned.
//! Not responsible for: user settings (see `config`).

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::ports::StateStore;
use crate::sync::MutexExt;

/// On-disk shape of `state.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    /// `"<model id>|<args hash>"` -> measured MiB.
    #[serde(default)]
    footprints: BTreeMap<String, u64>,
}

/// File-backed [`StateStore`].
pub struct FileStateStore {
    /// Location of `state.json`.
    path: PathBuf,
    /// In-memory copy, written back on every change.
    data: Mutex<StateFile>,
}

impl FileStateStore {
    /// Opens (or starts empty if missing or unreadable) the state file.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let data = fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        Self { path, data: Mutex::new(data) }
    }

    /// Key combining model id and memory-relevant settings.
    fn key(model: &str, args_hash: &str) -> String {
        format!("{model}|{args_hash}")
    }

    /// Writes the file atomically; failures are logged, never fatal,
    /// because the data is only a cache.
    fn persist(&self, data: &StateFile) {
        let Ok(json) = serde_json::to_string_pretty(data) else {
            return;
        };
        let tmp = self.path.with_extension("json.tmp");
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Err(e) = fs::write(&tmp, json).and_then(|()| fs::rename(&tmp, &self.path)) {
            warn!(path = %self.path.display(), error = %e, "could not save state.json");
        }
    }
}

impl StateStore for FileStateStore {
    fn footprint(&self, model: &str, args_hash: &str) -> Option<u64> {
        let data = self.data.lock_ok();
        data.footprints.get(&Self::key(model, args_hash)).copied()
    }

    fn set_footprint(&self, model: &str, args_hash: &str, mib: u64) {
        let mut data = self.data.lock_ok();
        // Drop older measurements of the same model: a changed setting
        // replaces the old figure rather than accumulating entries.
        let prefix = format!("{model}|");
        data.footprints.retain(|k, _| !k.starts_with(&prefix));
        data.footprints.insert(Self::key(model, args_hash), mib);
        self.persist(&data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footprints_survive_a_restart_and_replace_older_ones() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("state.json");
        {
            let s = FileStateStore::open(&path);
            assert_eq!(s.footprint("a", "h1"), None);
            s.set_footprint("a", "h1", 5000);
            s.set_footprint("b", "h9", 1000);
        }
        let s = FileStateStore::open(&path);
        assert_eq!(s.footprint("a", "h1"), Some(5000));
        s.set_footprint("a", "h2", 7000);
        assert_eq!(s.footprint("a", "h1"), None, "old measurement replaced");
        assert_eq!(s.footprint("a", "h2"), Some(7000));
        assert_eq!(s.footprint("b", "h9"), Some(1000));
    }

    #[test]
    fn corrupt_file_starts_empty() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("state.json");
        fs::write(&path, "{{{").expect("write");
        assert_eq!(FileStateStore::open(&path).footprint("a", "h"), None);
    }
}
