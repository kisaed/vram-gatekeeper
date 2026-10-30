//! In-flight request tracking (spec §8.6).
//!
//! Responsibility: the RAII [`InflightGuard`] that keeps a model's request
//! count honest, and the [`CancelHandle`] a drain uses to stop a request.
//! Not responsible for: deciding when to cancel (see `gate::controller`).
//!
//! The guard lives inside the response body, so it is released on normal
//! completion, errors, panics and client disconnects alike.
//!
//! Implements FR-GAME-3, FR-REQ-5.

use std::sync::{Arc, Mutex};

use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::gate::controller::Cmd;
use crate::gate::model_state::UnloadCause;
use crate::sync::MutexExt;

/// What cancelled a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelCause {
    /// The model is being unloaded for this reason.
    Unload(UnloadCause),
    /// The model's process crashed.
    Crash,
    /// Cancelled with no recorded reason.
    Cancelled,
}

impl CancelCause {
    /// Stable name shown to clients and logs: `game`, `overflow`, `crash`, ...
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unload(cause) => cause.as_str(),
            Self::Crash => "crash",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Why a request was cancelled; shown to the client (spec §8.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelInfo {
    /// What cancelled it.
    pub cause: CancelCause,
    /// Sentence for the error message.
    pub message: String,
}

/// Lets the controller cancel one request and lets the proxy learn why.
#[derive(Debug, Clone)]
pub struct CancelHandle {
    /// Fires when the request must stop.
    token: CancellationToken,
    /// Reason, set before the token fires.
    info: Arc<Mutex<Option<CancelInfo>>>,
}

impl Default for CancelHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelHandle {
    /// Creates an uncancelled handle.
    pub fn new() -> Self {
        Self { token: CancellationToken::new(), info: Arc::new(Mutex::new(None)) }
    }

    /// Cancels the request. The first reason wins.
    pub fn cancel(&self, info: CancelInfo) {
        {
            let mut slot = self.info.lock_ok();
            if slot.is_none() {
                *slot = Some(info);
            }
        }
        self.token.cancel();
    }

    /// Completes when the request has been cancelled.
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    /// True if already cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// The reason, once cancelled.
    pub fn info(&self) -> Option<CancelInfo> {
        self.info.lock_ok().clone()
    }

    /// The reason, or a generic one if the token fired without it.
    pub fn reason(&self) -> CancelInfo {
        self.info().unwrap_or_else(|| CancelInfo {
            cause: CancelCause::Cancelled,
            message: "The request was cancelled.".into(),
        })
    }
}

/// Counts one request against a model for as long as it is alive.
///
/// Creating it is the controller's job (it registers the request); dropping
/// it tells the controller the request is over.
pub struct InflightGuard {
    /// Canonical model id.
    model: Arc<str>,
    /// Request id within the controller.
    request_id: u64,
    /// Channel back to the controller.
    tx: UnboundedSender<Cmd>,
}

impl InflightGuard {
    /// Called by the controller when it admits a request.
    pub(crate) fn new(model: Arc<str>, request_id: u64, tx: UnboundedSender<Cmd>) -> Self {
        Self { model, request_id, tx }
    }

    /// The request id, for logging.
    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    /// The model this request counts against.
    pub fn model(&self) -> &str {
        &self.model
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        // A closed channel means the controller has stopped (shutdown), so
        // there is nobody left to tell.
        let _ = self.tx.send(Cmd::InflightDone { model: self.model.to_string(), request_id: self.request_id });
    }
}

impl std::fmt::Debug for InflightGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InflightGuard").field("model", &self.model).field("request_id", &self.request_id).finish()
    }
}
