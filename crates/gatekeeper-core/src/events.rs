//! The application event bus (spec §5.2).
//!
//! Responsibility: broadcast state changes to whoever listens (tray, UI
//! bridge, logs). Events are descriptive; listening never affects behaviour.
//! Not responsible for: persisting events.

use serde::Serialize;
use tokio::sync::broadcast;

use crate::config::validate::ValidationIssue;
use crate::gate::model_state::{ModelStateKind, UnloadCause};
use crate::gpu::VramSample;

/// Capacity of the broadcast channel. Slow listeners skip old events rather
/// than blocking the core.
const BUS_CAPACITY: usize = 256;

/// Kinds of desktop notification (maps to `ui.notifications`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// Game gate opened or closed.
    GameGate,
    /// Model unloaded for VRAM pressure.
    VramUnload,
    /// Model overflowed into system RAM.
    Overflow,
    /// Loop guard held a model.
    LoopGuard,
    /// Model failed to load.
    LoadFailure,
    /// A download finished or failed.
    Download,
}

/// Something that happened.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppEvent {
    /// A model changed lifecycle state (spec §7.2).
    ModelState {
        /// Model id.
        model: String,
        /// Previous state.
        from: ModelStateKind,
        /// New state.
        to: ModelStateKind,
        /// Unload cause, when relevant.
        cause: Option<UnloadCause>,
    },
    /// The gate opened or closed.
    Gate {
        /// True when the user paused the gatekeeper.
        paused: bool,
        /// Name of the game keeping the gate closed, if any.
        game: Option<String>,
        /// True if the user chose "allow anyway" for this session.
        override_active: bool,
    },
    /// A new GPU reading.
    Vram(VramSample),
    /// A line from a model's log.
    ModelLog {
        /// Model id.
        model: String,
        /// The line.
        line: String,
    },
    /// Something the user may want a notification for.
    Notification {
        /// Which setting governs it.
        kind: NotificationKind,
        /// Short title.
        title: String,
        /// Longer text.
        body: String,
    },
    /// A download changed: progress, state, or completion.
    Download(crate::hf::queue::DownloadInfo),
    /// The engine download changed: step, bytes, or the end of the install.
    EngineInstall(crate::engine::updater::InstallProgress),
    /// `config.json` was reloaded after a valid edit.
    ConfigReloaded,
    /// `config.json` was edited into an invalid state and not applied.
    ConfigError {
        /// Parse failure, if the file did not parse.
        parse_error: Option<String>,
        /// Validation problems with field paths.
        issues: Vec<ValidationIssue>,
    },
}

/// Cloneable handle for publishing and subscribing.
#[derive(Clone)]
pub struct EventBus {
    /// Underlying channel.
    tx: broadcast::Sender<AppEvent>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    /// Creates a bus.
    pub fn new() -> Self {
        Self { tx: broadcast::channel(BUS_CAPACITY).0 }
    }

    /// Publishes an event. Having no listeners is normal and not an error.
    pub fn publish(&self, event: AppEvent) {
        let _ = self.tx.send(event);
    }

    /// Starts listening.
    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.tx.subscribe()
    }
}
