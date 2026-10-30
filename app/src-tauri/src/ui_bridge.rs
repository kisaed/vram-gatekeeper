//! Forwards core events to the settings window while it exists (spec §17.1).
//!
//! Responsibility: subscribe to the core's event bus only while the window is
//! open and re-emit each event to it, so a closed window costs nothing.
//! Not responsible for: interpreting events (the UI does).

use gatekeeper_core::sync::MutexExt;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::broadcast::error::RecvError;
use tracing::warn;

use crate::state::HostState;
use crate::window::MAIN;

/// Name of the window event carrying an `AppEvent` as JSON.
pub const EVENT: &str = "core-event";

/// Name of the window event asking the page to re-read everything it shows.
pub const RESYNC_EVENT: &str = "core-resync";

/// Starts forwarding (replacing any earlier forwarder).
pub fn start(app: &AppHandle) {
    stop(app);
    let handle = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        let events = {
            let state = handle.state::<HostState>();
            let core = state.core.read().await;
            core.as_ref().map(|c| c.events.subscribe())
        };
        let Some(mut rx) = events else { return };
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if handle.emit_to(MAIN, EVENT, &event).is_err() {
                        break; // the window is gone
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    warn!(skipped = n, "the window could not keep up with core events");
                    // Tell the page to re-read everything it shows.
                    let _ = handle.emit_to(MAIN, RESYNC_EVENT, ());
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
    *app.state::<HostState>().bridge.lock_ok() = Some(task);
}

/// Called once startup has finished, with or without a core. A window that
/// opened first has no events and no status yet, so connect it now and tell
/// the page to read everything.
pub fn core_settled(app: &AppHandle) {
    if app.get_webview_window(MAIN).is_some() {
        start(app);
        let _ = app.emit_to(MAIN, RESYNC_EVENT, ());
    }
}

/// Stops forwarding.
pub fn stop(app: &AppHandle) {
    let task = app.state::<HostState>().bridge.lock_ok().take();
    if let Some(t) = task {
        t.abort();
    }
}
