//! The settings window: created on demand, destroyed on close
//! (spec §17.1, decision D2).
//!
//! Responsibility: open the window when asked and make sure closing it ends
//! the WebView2 process tree, so an idle tray app costs no WebView memory
//! (NFR-1).
//! Not responsible for: the page contents (see `app/ui`).

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tracing::{error, info};

use crate::ui_bridge;

/// Label of the one window.
pub const MAIN: &str = "main";

/// Shows the settings window, creating it if needed.
pub fn open(app: &AppHandle) {
    if let Some(existing) = app.get_webview_window(MAIN) {
        let _ = existing.unminimize();
        let _ = existing.show();
        let _ = existing.set_focus();
        return;
    }

    match WebviewWindowBuilder::new(app, MAIN, WebviewUrl::App("index.html".into()))
        .title("VRAM Gatekeeper")
        .inner_size(1120.0, 780.0)
        .min_inner_size(900.0, 600.0)
        .build()
    {
        Ok(window) => {
            info!("settings window opened");
            ui_bridge::start(app);
            let handle = app.clone();
            window.on_window_event(move |event| {
                // Closing a Tauri window destroys it, which tears down its
                // WebView2 process tree. Stop forwarding events when that happens.
                if matches!(event, WindowEvent::Destroyed) {
                    ui_bridge::stop(&handle);
                    info!("settings window closed");
                }
            });
        }
        Err(e) => {
            // Most likely the WebView2 runtime is missing. The core keeps running.
            error!(error = %e, "could not create the settings window");
            crate::tray::notify(app, "Cannot open the window", &format!("The WebView2 runtime may be missing: {e}"));
        }
    }
}
