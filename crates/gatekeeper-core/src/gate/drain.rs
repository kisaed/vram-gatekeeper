//! Drain planning (spec §7.4).
//!
//! Responsibility: decide, for an unload cause and the configured policy,
//! how in-flight requests are treated.
//! Not responsible for: executing the plan (timers and cancellation live in
//! `controller`).
//!
//! Implements FR-GAME-3, FR-VRAM-5.

use std::time::Duration;

use super::model_state::UnloadCause;
use crate::config::schema::{DrainPolicy, DrainingConfig};

/// What to do with requests still running when a model must unload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainPlan {
    /// Wait until in-flight reaches zero, with no limit.
    WaitForever,
    /// Wait up to the duration, then cancel what remains.
    WaitThenCancel(Duration),
    /// Cancel everything now.
    CancelNow,
}

/// Chooses the plan for `cause` under the configured policy.
pub fn plan(cause: UnloadCause, cfg: &DrainingConfig) -> DrainPlan {
    // The user chose "error" for overflow, so it never waits (spec §7.4).
    if matches!(cause, UnloadCause::Overflow) {
        return DrainPlan::CancelNow;
    }
    match cfg.policy {
        DrainPolicy::Finish => DrainPlan::WaitForever,
        DrainPolicy::WaitThenCancel => DrainPlan::WaitThenCancel(Duration::from_secs(u64::from(cfg.timeout_seconds))),
        DrainPolicy::Cancel => DrainPlan::CancelNow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(policy: DrainPolicy) -> DrainingConfig {
        DrainingConfig { policy, timeout_seconds: 7 }
    }

    #[test]
    fn follows_policy_for_normal_causes() {
        assert_eq!(plan(UnloadCause::Game, &cfg(DrainPolicy::Finish)), DrainPlan::WaitForever);
        assert_eq!(
            plan(UnloadCause::VramPressure, &cfg(DrainPolicy::WaitThenCancel)),
            DrainPlan::WaitThenCancel(Duration::from_secs(7))
        );
        assert_eq!(plan(UnloadCause::User, &cfg(DrainPolicy::Cancel)), DrainPlan::CancelNow);
    }

    #[test]
    fn overflow_always_cancels() {
        assert_eq!(plan(UnloadCause::Overflow, &cfg(DrainPolicy::Finish)), DrainPlan::CancelNow);
    }
}
