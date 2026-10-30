//! Why a request was refused (spec §8.5 reason codes).
//!
//! Responsibility: the structured reason the gate attaches to a refusal.
//! The proxy turns it into an OpenAI- or Anthropic-style error body.
//! Not responsible for: HTTP formatting (see `proxy::errors`).
//!
//! Implements FR-REQ-4.

use serde::Serialize;
use serde_json::{Value, json};

/// Machine-readable refusal reasons; serialized as the `code` clients see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectKind {
    /// Blocked by the game gate.
    GameRunning,
    /// Gatekeeper paused by the user (or the model is held by the user).
    Paused,
    /// Loop guard is holding the model.
    VramGuard,
    /// An in-flight request was cancelled.
    RequestCancelled,
    /// The model failed to start and is in backoff.
    ModelLoadFailed,
    /// The model did not become ready in time.
    LoadTimeout,
    /// llama-server exited unexpectedly.
    ModelCrashed,
    /// No enabled model has that id or alias.
    UnknownModel,
    /// The app is shutting down.
    ShuttingDown,
}

impl RejectKind {
    /// The `code` string clients see.
    pub fn code(self) -> &'static str {
        match self {
            Self::GameRunning => "game_running",
            Self::Paused => "paused",
            Self::VramGuard => "vram_guard",
            Self::RequestCancelled => "request_cancelled",
            Self::ModelLoadFailed => "model_load_failed",
            Self::LoadTimeout => "load_timeout",
            Self::ModelCrashed => "model_crashed",
            Self::UnknownModel => "unknown_model",
            Self::ShuttingDown => "shutting_down",
        }
    }
}

/// A refusal with everything needed to explain it.
#[derive(Debug, Clone)]
pub struct Rejection {
    /// Reason code.
    pub kind: RejectKind,
    /// Model the request named (or the canonical id).
    pub model: String,
    /// Sentence for humans.
    pub message: String,
    /// Extra machine-readable context, e.g. the game name.
    pub detail: Value,
    /// Seconds to suggest in `Retry-After`; `None` omits the header.
    pub retry_after_seconds: Option<u32>,
}

impl Rejection {
    /// Builds a rejection with empty detail and no retry hint.
    pub fn new(kind: RejectKind, model: impl Into<String>, message: impl Into<String>) -> Self {
        Self { kind, model: model.into(), message: message.into(), detail: json!({}), retry_after_seconds: None }
    }

    /// Adds detail.
    #[must_use]
    pub fn with_detail(mut self, detail: Value) -> Self {
        self.detail = detail;
        self
    }

    /// Adds a retry hint.
    #[must_use]
    pub fn with_retry_after(mut self, seconds: u32) -> Self {
        self.retry_after_seconds = Some(seconds);
        self
    }
}
