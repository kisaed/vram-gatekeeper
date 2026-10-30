//! Shared state of the Tauri host.
//!
//! Responsibility: hold the running core and the few bits of host bookkeeping
//! (startup error, the event-forwarding task) that commands, the tray and the
//! window code all need.
//! Not responsible for: any logic about models or games (that is the core).

use std::sync::Mutex;

use gatekeeper_core::app::RunningApp;
use serde::Serialize;
use tokio::sync::RwLock;

/// Managed by Tauri and handed to every command.
#[derive(Default)]
pub struct HostState {
    /// The running core; `None` until startup finishes or if it failed.
    pub core: RwLock<Option<RunningApp>>,
    /// Why startup failed, for the window to show.
    pub startup_error: Mutex<Option<String>>,
    /// Task forwarding core events to the open window.
    pub bridge: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

/// Error type of every command: a message plus, for validation failures, the
/// per-field problems so the form can highlight them (spec §17.5).
#[derive(Debug, Serialize)]
pub struct UiError {
    /// Plain-language message.
    pub message: String,
    /// Field-level problems, if any.
    pub issues: Vec<gatekeeper_core::config::ValidationIssue>,
    /// Machine-readable kind for errors the page reacts to.
    pub code: Option<&'static str>,
}

impl UiError {
    /// An error with only a message.
    pub fn msg(message: impl Into<String>) -> Self {
        Self { message: message.into(), issues: Vec::new(), code: None }
    }

    /// A validation failure: the first problem as the message, all of them as
    /// field issues, or `fallback` if the list is empty.
    pub fn from_issues(issues: Vec<gatekeeper_core::config::ValidationIssue>, fallback: &str) -> Self {
        let message = issues.first().map_or_else(|| fallback.to_owned(), |i| format!("{}: {}", i.path, i.message));
        Self { message, issues, code: None }
    }

    /// The "core is not running" error.
    pub fn not_running() -> Self {
        Self::msg("The gatekeeper core is not running. Check the startup message on the Status page.")
    }
}

impl From<gatekeeper_core::hf::api::HfError> for UiError {
    fn from(e: gatekeeper_core::hf::api::HfError) -> Self {
        let code = (e == gatekeeper_core::hf::api::HfError::NeedsAccess).then_some("needs_access");
        Self { code, ..Self::msg(e.to_string()) }
    }
}

impl From<String> for UiError {
    fn from(message: String) -> Self {
        Self::msg(message)
    }
}

impl From<gatekeeper_core::CoreError> for UiError {
    fn from(e: gatekeeper_core::CoreError) -> Self {
        Self::msg(e.to_string())
    }
}
