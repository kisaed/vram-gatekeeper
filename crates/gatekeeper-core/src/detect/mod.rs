//! Game detection (spec §11).
//!
//! Responsibility: the [`GameSignal`] message detectors send to the gate
//! controller. Detectors report observations; they never decide policy.
//! Not responsible for: the cooldown or what happens to models (see
//! `gate::controller`).
//!
//! Implements FR-GAME-1.

/// One detector's current verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameSignal {
    /// Which detector reported, e.g. `process_list` or `steam`. The gate
    /// stays closed until every source that said "active" says "inactive".
    pub source: String,
    /// True while this source sees a game.
    pub active: bool,
    /// Display name of the game, e.g. `eldenring.exe`.
    pub name: Option<String>,
}

impl GameSignal {
    /// Convenience constructor for an "active" report.
    pub fn active(source: &str, name: &str) -> Self {
        Self { source: source.to_owned(), active: true, name: Some(name.to_owned()) }
    }

    /// Convenience constructor for an "inactive" report.
    pub fn inactive(source: &str) -> Self {
        Self { source: source.to_owned(), active: false, name: None }
    }
}

use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use crate::config::schema::Config;
use crate::error::CoreResult;

#[cfg(windows)]
pub mod foreground;
pub mod game_list;
#[cfg(windows)]
pub mod heavy_gpu;
pub mod process_scan;
#[cfg(windows)]
pub mod steam;
#[cfg(windows)]
pub mod windows_games;

/// A source of "is a game running?" signals (spec §11).
///
/// Detectors push observations; they never decide policy. Each observation
/// names its source so the UI can explain *why* the gate closed.
pub trait GameDetector: Send {
    /// Stable source name used in [`GameSignal::source`].
    fn name(&self) -> &'static str;

    /// Starts observing and sending signals.
    ///
    /// # Errors
    /// Returns an error if the detector cannot initialise; the app logs it
    /// and continues with the remaining detectors.
    fn start(
        &mut self,
        signals: mpsc::UnboundedSender<GameSignal>,
        config: watch::Receiver<Arc<Config>>,
    ) -> CoreResult<()>;

    /// Stops observing.
    fn stop(&mut self);
}
