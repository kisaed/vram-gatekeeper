//! In-memory log ring buffers for model processes (spec §9.4).
//!
//! Responsibility: keep the last lines of each model's stdout/stderr for the
//! GUI log view and for crash reports.
//! Not responsible for: writing log files (the launcher tees lines to disk).

use super::log_parse::parse_speculative_implementation;
use crate::sync::MutexExt;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// Lines kept per model (spec §9.4).
pub const LOG_RING_LINES: usize = 2_000;

/// Recent lines of one model, and what they said about speculative decoding.
#[derive(Default)]
struct ModelLog {
    /// The newest lines, oldest first.
    lines: VecDeque<String>,
    /// Speculative types the engine reported starting since the last launch.
    /// Kept apart from `lines` because start-up prints far more than the ring
    /// holds, and the line could be evicted before the model is ready.
    speculative: Vec<String>,
}

/// Shared, cloneable store of recent log lines per model id.
#[derive(Clone, Default)]
pub struct LogStore {
    /// `model id -> log`.
    inner: Arc<Mutex<HashMap<String, ModelLog>>>,
}

impl LogStore {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a line, evicting the oldest beyond [`LOG_RING_LINES`].
    pub fn push(&self, model: &str, line: String) {
        let mut map = self.inner.lock_ok();
        let log = map.entry(model.to_owned()).or_default();
        if let Some(ty) = parse_speculative_implementation(&line)
            && !log.speculative.contains(&ty)
        {
            log.speculative.push(ty);
        }
        if log.lines.len() >= LOG_RING_LINES {
            log.lines.pop_front();
        }
        log.lines.push_back(line);
    }

    /// Speculative types the engine reported starting since the last launch of `model`.
    pub fn speculative_started(&self, model: &str) -> Vec<String> {
        self.inner.lock_ok().get(model).map_or_else(Vec::new, |l| l.speculative.clone())
    }

    /// The last `n` lines for `model`, oldest first.
    pub fn tail(&self, model: &str, n: usize) -> Vec<String> {
        let map = self.inner.lock_ok();
        map.get(model)
            .map_or_else(Vec::new, |l| l.lines.iter().skip(l.lines.len().saturating_sub(n)).cloned().collect())
    }

    /// Forgets a model's lines (called when a new launch begins).
    pub fn clear(&self, model: &str) {
        let mut map = self.inner.lock_ok();
        map.remove(model);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_newest_lines() {
        let s = LogStore::new();
        for i in 0..(LOG_RING_LINES + 5) {
            s.push("m", format!("line {i}"));
        }
        let all = s.tail("m", usize::MAX);
        assert_eq!(all.len(), LOG_RING_LINES);
        assert_eq!(all[0], "line 5");
        assert_eq!(s.tail("m", 2), ["line 2003", "line 2004"]);
        assert!(s.tail("other", 5).is_empty());
    }

    #[test]
    fn remembers_started_speculative_types_only_for_the_current_launch() {
        let s = LogStore::new();
        s.push("m", "x: adding speculative implementation 'draft-simple'".into());
        s.push("m", "x: adding speculative implementation 'draft-simple'".into());
        // Start-up logs can be longer than the ring; the fact must survive that.
        for i in 0..(LOG_RING_LINES + 10) {
            s.push("m", format!("line {i}"));
        }
        assert_eq!(s.speculative_started("m"), ["draft-simple"], "seen once, kept after eviction");
        assert!(s.speculative_started("other").is_empty());
        s.clear("m");
        assert!(s.speculative_started("m").is_empty(), "a new launch starts from nothing");
    }
}
