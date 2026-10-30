//! Pure VRAM decision logic (spec §13).
//!
//! Responsibility: the arithmetic and selection rules for pressure, spill,
//! layer overflow and make-room, kept free of I/O and timers so every rule
//! is a plain unit test. The controller feeds in observations and applies
//! the answers.
//! Not responsible for: sampling the GPU (`gpu`) or unloading (`controller`).
//!
//! Implements FR-VRAM-1, FR-VRAM-2, FR-VRAM-4, FR-VRAM-7.

use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;

use crate::config::schema::PressureScope;
use crate::gpu::BYTES_PER_MIB;

/// Consecutive samples above the spill threshold needed to call it spill;
/// filters one-off counter glitches (spec §13.2).
pub const SPILL_CONSECUTIVE_SAMPLES: u32 = 3;

/// Time after `Ready` before the footprint and spill baseline are read,
/// giving the driver time to finish allocating (spec §9.5).
pub const BASELINE_DELAY: Duration = Duration::from_secs(5);

/// Percent of a model's footprint that must disappear from adapter usage
/// before a stop counts as "released" (spec §9.2). Not 100 because other
/// applications' usage moves at the same time.
pub const RELEASE_FRACTION_PERCENT: u64 = 80;

/// Compute-buffer margin added to file-size estimates, in MiB (spec §9.5).
pub const ESTIMATE_COMPUTE_MARGIN_MIB: u64 = 512;

/// Percentage of adapter VRAM in use, rounded down. Zero total reads as 0.
pub fn pressure_percent(used_mib: u64, total_mib: u64) -> u32 {
    if total_mib == 0 {
        return 0;
    }
    u32::try_from(used_mib.saturating_mul(100) / total_mib).unwrap_or(u32::MAX)
}

/// Tracks how long pressure has persisted.
#[derive(Debug, Default, Clone)]
pub struct PressureTracker {
    /// When the current above-threshold streak began.
    since: Option<Instant>,
}

impl PressureTracker {
    /// Feeds one observation. Returns true when pressure has lasted at least
    /// `sustain`, at which point the streak resets so the caller acts once.
    pub fn observe(&mut self, now: Instant, above: bool, sustain: Duration) -> bool {
        if !above {
            self.since = None;
            return false;
        }
        let start = *self.since.get_or_insert(now);
        if now.duration_since(start) >= sustain {
            self.since = None;
            return true;
        }
        false
    }

    /// Discards the current streak.
    pub fn reset(&mut self) {
        self.since = None;
    }
}

/// A loaded model considered for eviction.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Model id.
    pub id: String,
    /// Pinned models are evicted last (pressure) or never (make-room).
    pub pinned: bool,
    /// Last time the model served or started a request.
    pub last_used: Instant,
    /// Footprint in MiB, used to stop evicting once enough is freed.
    pub footprint_mib: u64,
}

/// Picks models to unload under pressure (spec §13.1): unpinned before
/// pinned, then least recently used first.
pub fn pick_pressure_victims(mut candidates: Vec<Candidate>, scope: PressureScope) -> Vec<String> {
    candidates.sort_by_key(|c| (c.pinned, c.last_used));
    match scope {
        // One at a time; the controller re-samples after the release.
        PressureScope::LruUntilBelow => candidates.into_iter().take(1).map(|c| c.id).collect(),
        PressureScope::All => candidates.into_iter().map(|c| c.id).collect(),
    }
}

/// Picks idle, unpinned models to evict so that `need_mib` fits in
/// `free_mib` (spec §13.4). Returns an empty list when nothing can help.
pub fn pick_make_room_victims(mut idle_candidates: Vec<Candidate>, free_mib: u64, need_mib: u64) -> Vec<String> {
    idle_candidates.retain(|c| !c.pinned);
    idle_candidates.sort_by_key(|c| c.last_used);
    let mut freed = free_mib;
    let mut victims = Vec::new();
    for c in idle_candidates {
        if freed >= need_mib {
            break;
        }
        freed = freed.saturating_add(c.footprint_mib);
        victims.push(c.id);
    }
    if freed >= need_mib { victims } else { Vec::new() }
}

/// Counts consecutive spill observations for one model.
#[derive(Debug, Default, Clone)]
pub struct SpillDetector {
    /// Samples in a row above the threshold.
    streak: u32,
}

impl SpillDetector {
    /// Feeds the model's current shared-memory use. True once the growth over
    /// `baseline_mib` has exceeded `threshold_mib` for enough samples.
    pub fn observe(&mut self, shared_mib: u64, baseline_mib: u64, threshold_mib: u64) -> bool {
        if shared_mib.saturating_sub(baseline_mib) > threshold_mib {
            self.streak += 1;
        } else {
            self.streak = 0;
        }
        self.streak >= SPILL_CONSECUTIVE_SAMPLES
    }
}

/// Decides whether a GPU layer placement counts as overflow (spec §13.2
/// check 1): fewer than all layers on the GPU **and** the user did not ask
/// for a specific number, so llama.cpp's auto-fit moved them.
pub fn layers_overflow(offloaded: u32, total: u32, n_gpu_layers_setting: Option<&Value>) -> bool {
    if offloaded >= total {
        return false;
    }
    match n_gpu_layers_setting {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => matches!(s.trim().to_ascii_lowercase().as_str(), "auto" | "all"),
        // An explicit number is the user's own split, not a failure.
        Some(_) => false,
    }
}

/// Estimates a model's VRAM need in MiB from its file size alone, until a
/// real measurement exists (spec §9.5).
pub fn estimate_footprint_mib(file_bytes: u64) -> u64 {
    file_bytes / BYTES_PER_MIB + ESTIMATE_COMPUTE_MARGIN_MIB
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, pinned: bool, age_secs: u64, mib: u64, base: Instant) -> Candidate {
        Candidate { id: id.into(), pinned, last_used: base + Duration::from_secs(age_secs), footprint_mib: mib }
    }

    #[test]
    fn pressure_percent_rounds_down_and_handles_zero_total() {
        assert_eq!(pressure_percent(90, 100), 90);
        assert_eq!(pressure_percent(29_999, 32_000), 93);
        assert_eq!(pressure_percent(5, 0), 0);
    }

    #[test]
    fn pressure_needs_sustain_and_resets_on_dip() {
        let t0 = Instant::now();
        let s = Duration::from_secs(3);
        let mut p = PressureTracker::default();
        assert!(!p.observe(t0, true, s));
        assert!(!p.observe(t0 + Duration::from_secs(2), true, s));
        // A dip below threshold restarts the clock (shader-compile spike).
        assert!(!p.observe(t0 + Duration::from_secs(2), false, s));
        assert!(!p.observe(t0 + Duration::from_secs(4), true, s));
        assert!(p.observe(t0 + Duration::from_secs(7), true, s));
        // Fired once; the next observation starts a new streak.
        assert!(!p.observe(t0 + Duration::from_secs(8), true, s));
    }

    #[test]
    fn lru_picks_oldest_unpinned_first() {
        let t0 = Instant::now();
        let c = vec![cand("pinned-old", true, 0, 1, t0), cand("new", false, 50, 1, t0), cand("old", false, 10, 1, t0)];
        assert_eq!(pick_pressure_victims(c.clone(), PressureScope::LruUntilBelow), ["old"]);
        assert_eq!(pick_pressure_victims(c, PressureScope::All), ["old", "new", "pinned-old"]);
    }

    #[test]
    fn make_room_evicts_only_as_many_as_needed_and_never_pinned() {
        let t0 = Instant::now();
        let c = vec![cand("a", false, 1, 4000, t0), cand("b", false, 2, 4000, t0), cand("p", true, 0, 20_000, t0)];
        assert_eq!(pick_make_room_victims(c.clone(), 1000, 5000), ["a"]);
        assert_eq!(pick_make_room_victims(c.clone(), 1000, 9000), ["a", "b"]);
        // Even evicting everything unpinned would not fit: evict nothing.
        assert!(pick_make_room_victims(c, 1000, 20_000).is_empty());
    }

    #[test]
    fn spill_needs_consecutive_samples_over_baseline() {
        let mut s = SpillDetector::default();
        assert!(!s.observe(1000, 100, 512));
        assert!(!s.observe(1000, 100, 512));
        assert!(s.observe(1000, 100, 512));
        let mut s = SpillDetector::default();
        assert!(!s.observe(1000, 100, 512));
        assert!(!s.observe(200, 100, 512)); // a dip resets the streak
        assert!(!s.observe(1000, 100, 512));
    }

    #[test]
    fn layer_overflow_respects_user_choice() {
        let auto = Value::from("auto");
        assert!(layers_overflow(20, 33, None));
        assert!(layers_overflow(20, 33, Some(&auto)));
        assert!(!layers_overflow(33, 33, Some(&auto)));
        assert!(!layers_overflow(20, 33, Some(&Value::from(20))));
    }

    #[test]
    fn estimate_adds_margin() {
        assert_eq!(estimate_footprint_mib(4 * 1024 * 1024 * 1024), 4096 + 512);
    }
}
