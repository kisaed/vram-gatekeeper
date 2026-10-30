//! Sliding-window counter behind the loop guard (spec §13.3).
//!
//! Responsibility: remember when a model was unloaded for VRAM pressure or
//! overflow, and say whether that happened often enough to hold the model.
//! Not responsible for: deciding what "held" means for requests (see
//! `controller`) or measuring free VRAM (see `vram_rules`).
//!
//! Implements FR-VRAM-6.

use std::collections::VecDeque;
use std::time::Duration;

use tokio::time::Instant;

/// Unload history for one model.
#[derive(Debug, Default, Clone)]
pub struct LoopGuard {
    /// Times of recent pressure/overflow unloads, oldest first.
    events: VecDeque<Instant>,
}

impl LoopGuard {
    /// Records an unload at `now` and forgets events older than `window`.
    pub fn record(&mut self, now: Instant, window: Duration) {
        self.events.push_back(now);
        self.prune(now, window);
    }

    /// Number of unloads inside the window ending at `now`.
    pub fn count(&mut self, now: Instant, window: Duration) -> usize {
        self.prune(now, window);
        self.events.len()
    }

    /// True once `max_unloads` or more unloads fall inside the window.
    pub fn tripped(&mut self, now: Instant, window: Duration, max_unloads: u32) -> bool {
        self.count(now, window) >= max_unloads as usize
    }

    /// Forgets all history; called when a hold is released.
    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Drops events at or beyond the window edge.
    fn prune(&mut self, now: Instant, window: Duration) {
        while let Some(&first) = self.events.front() {
            if now.duration_since(first) >= window {
                self.events.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Duration = Duration::from_secs(600);

    #[test]
    fn trips_at_max_inside_window() {
        let t0 = Instant::now();
        let mut g = LoopGuard::default();
        g.record(t0, WINDOW);
        g.record(t0 + Duration::from_secs(60), WINDOW);
        assert!(!g.tripped(t0 + Duration::from_secs(61), WINDOW, 3));
        g.record(t0 + Duration::from_secs(120), WINDOW);
        assert!(g.tripped(t0 + Duration::from_secs(121), WINDOW, 3));
    }

    #[test]
    fn old_events_expire() {
        let t0 = Instant::now();
        let mut g = LoopGuard::default();
        g.record(t0, WINDOW);
        g.record(t0 + Duration::from_secs(10), WINDOW);
        assert_eq!(g.count(t0 + Duration::from_secs(605), WINDOW), 1);
        assert_eq!(g.count(t0 + Duration::from_secs(700), WINDOW), 0);
    }

    #[test]
    fn clear_resets() {
        let t0 = Instant::now();
        let mut g = LoopGuard::default();
        g.record(t0, WINDOW);
        g.clear();
        assert_eq!(g.count(t0, WINDOW), 0);
    }
}
