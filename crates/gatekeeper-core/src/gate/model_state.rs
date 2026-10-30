//! Per-model lifecycle state (spec §7.2).
//!
//! Responsibility: the [`ModelState`] enum, the [`UnloadCause`] that
//! explains why a model is going away, and the runtime record the
//! controller keeps for each model.
//! Not responsible for: transition decisions (see `controller`).
//!
//! Implements FR-GAME-4, FR-VRAM-3.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::oneshot;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::loop_guard::LoopGuard;
use super::rejection::Rejection;
use super::vram_rules::SpillDetector;
use crate::ports::{ProcessKiller, Upstream};
use crate::proxy::inflight::CancelHandle;

/// Why a model is being unloaded (spec §7.2 table). The cause decides what
/// happens to new requests and whether the loop guard counts the unload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnloadCause {
    /// A game started: new requests are rejected.
    Game,
    /// The user paused the gatekeeper: new requests are rejected.
    Pause,
    /// VRAM pressure: new requests queue and reload.
    VramPressure,
    /// Overflow into system RAM with the "error" action.
    Overflow,
    /// Idle timer elapsed.
    IdleTtl,
    /// Evicted to make room for another model.
    MakeRoom,
    /// The user asked for it.
    User,
    /// A restart-required setting changed.
    ConfigChange,
    /// The app is exiting.
    Shutdown,
}

impl UnloadCause {
    /// Stable name for logs and events.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Game => "game",
            Self::Pause => "pause",
            Self::VramPressure => "vram_pressure",
            Self::Overflow => "overflow",
            Self::IdleTtl => "idle_ttl",
            Self::MakeRoom => "make_room",
            Self::User => "user",
            Self::ConfigChange => "config_change",
            Self::Shutdown => "shutdown",
        }
    }

    /// True for causes the loop guard counts (spec §13.3).
    pub fn counts_for_loop_guard(self) -> bool {
        matches!(self, Self::VramPressure | Self::Overflow)
    }

    /// True if requests arriving during the unload are rejected rather than
    /// queued for a reload (spec §7.2 table).
    pub fn rejects_new_requests(self) -> bool {
        matches!(self, Self::Game | Self::Pause | Self::Shutdown)
    }
}

/// The lifecycle state without its payload; this is what status snapshots,
/// events and the UI carry (spec §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelStateKind {
    /// No process.
    Unloaded,
    /// Process launched; waiting for `/health`.
    Starting,
    /// Serving requests.
    Ready,
    /// Waiting for (or cancelling) in-flight requests before the kill.
    Draining,
    /// Process being killed and VRAM released.
    Stopping,
    /// Loop guard is holding the model.
    Held,
    /// Failed to start; in backoff.
    Failed,
}

impl ModelStateKind {
    /// Stable name for logs and events; the same string the JSON carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unloaded => "unloaded",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Draining => "draining",
            Self::Stopping => "stopping",
            Self::Held => "held",
            Self::Failed => "failed",
        }
    }

    /// True while a process exists or is being created.
    pub fn has_process(self) -> bool {
        matches!(self, Self::Starting | Self::Ready | Self::Draining | Self::Stopping)
    }
}

impl std::fmt::Display for ModelStateKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The gate in one word, shared by the tray icon and the window. A more
/// urgent condition wins, in the order the variants appear here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateIndicator {
    /// Paused by the user.
    Paused,
    /// The loop guard is holding a model.
    Held,
    /// Draining a model before unloading it.
    Draining,
    /// Closed for a game.
    Closed,
    /// A model is loading or serving.
    Busy,
    /// Open, models available.
    Open,
}

impl GateIndicator {
    /// Derives the indicator from the gate flags and every model's state.
    pub fn derive(paused: bool, game_blocks: bool, states: impl IntoIterator<Item = ModelStateKind> + Clone) -> Self {
        let any = |kinds: &[ModelStateKind]| states.clone().into_iter().any(|s| kinds.contains(&s));
        if paused {
            Self::Paused
        } else if any(&[ModelStateKind::Held]) {
            Self::Held
        } else if any(&[ModelStateKind::Draining]) {
            Self::Draining
        } else if game_blocks {
            Self::Closed
        } else if any(&[ModelStateKind::Starting, ModelStateKind::Ready, ModelStateKind::Stopping]) {
            Self::Busy
        } else {
            Self::Open
        }
    }
}

/// Lifecycle state of one model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelState {
    /// No process.
    Unloaded,
    /// Process launched; waiting for `/health`.
    Starting,
    /// Serving requests.
    Ready,
    /// Waiting for (or cancelling) in-flight requests before the kill.
    Draining(UnloadCause),
    /// Process being killed and VRAM released.
    Stopping(UnloadCause),
    /// Loop guard is holding the model (spec §13.3).
    Held,
    /// Failed to start; retry allowed once `until` passes.
    Failed {
        /// End of the backoff.
        until: Instant,
        /// Last error for display.
        message: String,
    },
}

impl ModelState {
    /// The state without its payload.
    pub fn kind(&self) -> ModelStateKind {
        match self {
            Self::Unloaded => ModelStateKind::Unloaded,
            Self::Starting => ModelStateKind::Starting,
            Self::Ready => ModelStateKind::Ready,
            Self::Draining(_) => ModelStateKind::Draining,
            Self::Stopping(_) => ModelStateKind::Stopping,
            Self::Held => ModelStateKind::Held,
            Self::Failed { .. } => ModelStateKind::Failed,
        }
    }

    /// Stable name for logs and events.
    pub fn name(&self) -> &'static str {
        self.kind().as_str()
    }

    /// The unload cause, when draining or stopping.
    pub fn cause(&self) -> Option<UnloadCause> {
        match self {
            Self::Draining(c) | Self::Stopping(c) => Some(*c),
            _ => None,
        }
    }

    /// True while a process exists or is being created.
    pub fn has_process(&self) -> bool {
        self.kind().has_process()
    }
}

/// A request waiting for its model to become ready.
pub struct Waiter {
    /// Id used by the timeout message.
    pub id: u64,
    /// Where the verdict goes.
    pub reply: oneshot::Sender<Result<super::controller::Admitted, Rejection>>,
}

/// Handle to the running child, owned by the controller.
pub struct Run {
    /// Launch generation; events carrying another number are stale.
    pub generation: u64,
    /// OS process id (known once spawned).
    pub pid: Option<u32>,
    /// Where the child listens (known once spawned).
    pub upstream: Option<Upstream>,
    /// Kills the child (known once spawned).
    pub killer: Option<Arc<dyn ProcessKiller>>,
    /// Cancelled to stop the start task's health polling.
    pub stop_start_task: CancellationToken,
    /// Set if a kill was requested before the child had spawned.
    pub kill_when_spawned: bool,
}

impl Run {
    /// Stops the start task and kills the child if it has spawned.
    pub fn abort(self) {
        self.stop_start_task.cancel();
        if let Some(k) = self.killer {
            k.kill();
        }
    }
}

/// Everything the controller tracks for one model.
pub struct ModelRt {
    /// Lifecycle state.
    pub state: ModelState,
    /// Current child, if any.
    pub run: Option<Run>,
    /// Requests being served, by request id.
    pub inflight: HashMap<u64, CancelHandle>,
    /// Requests waiting for readiness.
    pub waiters: Vec<Waiter>,
    /// Last time the model was requested or finished a request.
    pub last_used: Instant,
    /// When the current child became ready.
    pub ready_at: Option<Instant>,
    /// Measured or estimated VRAM footprint in MiB.
    pub footprint_mib: Option<u64>,
    /// True while `footprint_mib` is an estimate.
    pub footprint_estimated: bool,
    /// Shared-memory use right after load; reference for spill detection.
    pub baseline_shared_mib: Option<u64>,
    /// Spill detection state.
    pub spill: SpillDetector,
    /// GPU layer placement `(offloaded, total)` reported at load.
    pub layers: Option<(u32, u32)>,
    /// Description of detected overflow, if any.
    pub overflow: Option<String>,
    /// Loop-guard history.
    pub loop_guard: LoopGuard,
    /// When free VRAM first satisfied the release condition (held models).
    pub held_ok_since: Option<Instant>,
    /// Held by the user's "unload and hold" (spec §7.1 `ManualUnloadHold`).
    pub manual_hold: bool,
    /// Consecutive start failures, for exponential backoff.
    pub fail_count: u32,
    /// Start again as soon as the current stop completes.
    pub reload_after_stop: bool,
    /// Fingerprint of launch settings the child was started with.
    pub launched_fingerprint: Option<String>,
    /// Settings changed since launch; restart needed.
    pub needs_restart: bool,
    /// Hash identifying memory-relevant settings, keying the footprint.
    pub args_hash: String,
    /// Restarts already spent on port collisions during this start.
    pub port_retries: u32,
    /// Waiting for evictions before starting (make-room).
    pub awaiting_room: bool,
    /// Pending timer id for the drain timeout; stale timers are ignored.
    pub drain_timer: u64,
    /// Pending timer id for the release wait.
    pub release_timer: u64,
    /// Adapter usage when the child exited, for release detection.
    pub release_baseline_used_mib: Option<u64>,
}

impl ModelRt {
    /// Creates the record for a model that has never run.
    pub fn new(now: Instant) -> Self {
        Self {
            state: ModelState::Unloaded,
            run: None,
            inflight: HashMap::new(),
            waiters: Vec::new(),
            last_used: now,
            ready_at: None,
            footprint_mib: None,
            footprint_estimated: false,
            baseline_shared_mib: None,
            spill: SpillDetector::default(),
            layers: None,
            overflow: None,
            loop_guard: LoopGuard::default(),
            held_ok_since: None,
            manual_hold: false,
            fail_count: 0,
            reload_after_stop: false,
            launched_fingerprint: None,
            needs_restart: false,
            args_hash: String::new(),
            port_retries: 0,
            awaiting_room: false,
            drain_timer: 0,
            release_timer: 0,
            release_baseline_used_mib: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ModelStateKind::{Draining, Failed, Held, Ready, Starting, Stopping, Unloaded};
    use super::*;

    fn gate(paused: bool, game_blocks: bool, states: &[ModelStateKind]) -> GateIndicator {
        GateIndicator::derive(paused, game_blocks, states.iter().copied())
    }

    #[test]
    fn idle_is_open_and_a_running_model_is_busy() {
        assert_eq!(gate(false, false, &[Unloaded, Failed]), GateIndicator::Open);
        assert_eq!(gate(false, false, &[Ready]), GateIndicator::Busy);
        assert_eq!(gate(false, false, &[Starting]), GateIndicator::Busy);
        assert_eq!(gate(false, false, &[Stopping]), GateIndicator::Busy);
    }

    #[test]
    fn urgent_conditions_win_in_order() {
        assert_eq!(gate(false, true, &[Ready, Draining]), GateIndicator::Draining, "draining beats the closed gate");
        assert_eq!(gate(false, true, &[Draining, Held]), GateIndicator::Held);
        assert_eq!(gate(true, true, &[Draining, Held]), GateIndicator::Paused, "paused beats everything");
    }

    #[test]
    fn a_game_closes_the_gate_until_overridden() {
        assert_eq!(gate(false, true, &[Unloaded]), GateIndicator::Closed);
        assert_eq!(gate(false, false, &[Unloaded]), GateIndicator::Open);
    }
}
