//! The tray icon and its menu (spec §17.2, FR-APP-1, FR-APP-3, FR-APP-4).
//!
//! Responsibility: show gate state at a glance, offer the manual controls, and
//! raise desktop notifications. The tray exists for the whole life of the app;
//! it is native (no WebView), so it is the only UI an idle app has.
//! Not responsible for: deciding states (see `tray_model`).

use std::sync::Mutex;

use gatekeeper_core::events::{AppEvent, NotificationKind};
use gatekeeper_core::gate::controller::StatusSnapshot;
use gatekeeper_core::gate::model_state::{GateIndicator, ModelStateKind};
use gatekeeper_core::sync::MutexExt;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::broadcast::error::RecvError;
use tracing::warn;

use crate::state::HostState;
use crate::tray_icons::icon_for;
use crate::tray_model::{MenuAvailability, inflight_total, menu_availability, tooltip};
use crate::window;

/// Id of the single tray icon.
const TRAY_ID: &str = "main";

/// Creates the tray icon in its "starting" state.
///
/// # Errors
/// Returns an error if Windows refuses to create the tray icon.
pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app, None)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon_for(GateIndicator::Busy))
        .tooltip("VRAM Gatekeeper - starting")
        .menu(&menu)
        // Left click opens the window (spec §17.2); the menu stays on right click.
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                window::open(tray.app_handle());
            }
        })
        .on_menu_event(|app, event| on_menu(app, event.id.as_ref()))
        .build(app)?;
    Ok(())
}

/// Builds the menu for the given status (`None` while starting).
fn build_menu(app: &AppHandle, status: Option<&StatusSnapshot>) -> tauri::Result<Menu<tauri::Wry>> {
    let avail = status.map(menu_availability);
    let status_line = status.map_or_else(|| "Starting...".to_owned(), tooltip);
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(app, "status", status_line, false, None::<&str>)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "open", "Open VRAM Gatekeeper", true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(
        app,
        "unload_all",
        "Unload all models now",
        avail.is_some_and(|a| a.unload_all),
        None::<&str>,
    )?)?;
    if avail.is_some_and(|a| a.allow_anyway) {
        menu.append(&MenuItem::with_id(
            app,
            "allow_anyway",
            "Allow anyway (until this game closes)",
            true,
            None::<&str>,
        )?)?;
    }
    if avail.is_some_and(|a| a.release_held) {
        menu.append(&MenuItem::with_id(app, "release", "Release held models", true, None::<&str>)?)?;
    }
    let pause_label = if avail.is_some_and(|a| a.resume) { "Resume gatekeeper" } else { "Pause gatekeeper" };
    menu.append(&MenuItem::with_id(app, "pause_toggle", pause_label, avail.is_some(), None::<&str>)?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?)?;
    Ok(menu)
}

/// What the tray last showed; one model load fires several events, most of
/// which change nothing visible.
type Shown = (GateIndicator, String, MenuAvailability);

/// Applies a status snapshot to the tray: icon, tooltip and menu. Does
/// nothing when the tray already shows exactly that.
fn apply(app: &AppHandle, status: &StatusSnapshot) {
    static LAST: Mutex<Option<Shown>> = Mutex::new(None);
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };
    let shown: Shown = (status.gate, tooltip(status), menu_availability(status));
    let mut last = LAST.lock_ok();
    if last.as_ref() == Some(&shown) {
        return;
    }
    let _ = tray.set_icon(Some(icon_for(shown.0)));
    let _ = tray.set_tooltip(Some(shown.1.clone()));
    if let Ok(menu) = build_menu(app, Some(status)) {
        let _ = tray.set_menu(Some(menu));
    }
    *last = Some(shown);
}

/// Handles a click on a menu item.
fn on_menu(app: &AppHandle, id: &str) {
    let app = app.clone();
    let id = id.to_owned();
    tauri::async_runtime::spawn(async move {
        if id == "open" {
            window::open(&app);
            return;
        }
        if id == "quit" {
            quit(&app).await;
            return;
        }
        let state = app.state::<HostState>();
        let core = state.core.read().await;
        let Some(core) = core.as_ref() else { return };
        match id.as_str() {
            "unload_all" => core.controller.unload_all(),
            "allow_anyway" => core.controller.allow_anyway(),
            "release" => {
                if let Some(status) = core.controller.status().await {
                    for m in status.models.iter().filter(|m| m.state == ModelStateKind::Held || m.manual_hold) {
                        core.controller.release_hold(&m.id);
                    }
                }
            }
            "pause_toggle" => {
                let paused = core.controller.status().await.is_some_and(|s| s.paused);
                core.controller.set_paused(!paused);
            }
            _ => {}
        }
    });
}

/// Quits, asking first if requests are still running (spec §17.2).
async fn quit(app: &AppHandle) {
    let in_flight = {
        let state = app.state::<HostState>();
        let core = state.core.read().await;
        match core.as_ref() {
            Some(c) => c.controller.status().await.map_or(0, |s| inflight_total(&s)),
            None => 0,
        }
    };
    if in_flight > 0 {
        let (tx, rx) = tokio::sync::oneshot::channel();
        app.dialog()
            .message(format!("{in_flight} request(s) are still running and will be cancelled. Quit anyway?"))
            .title("Quit VRAM Gatekeeper")
            .buttons(MessageDialogButtons::OkCancelCustom("Quit".into(), "Keep running".into()))
            .show(move |confirmed| {
                let _ = tx.send(confirmed);
            });
        if !rx.await.unwrap_or(false) {
            return;
        }
    }
    app.exit(0);
}

/// Shows a desktop notification.
pub fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        warn!(error = %e, "could not show a notification");
    }
}

/// Whether the user enabled notifications of this kind (`ui.notifications`).
fn enabled(core: &gatekeeper_core::app::RunningApp, kind: NotificationKind) -> bool {
    let n = core.config().ui.notifications.clone();
    match kind {
        NotificationKind::GameGate => n.game_gate,
        NotificationKind::VramUnload => n.vram_unload,
        NotificationKind::Overflow => n.overflow,
        NotificationKind::LoopGuard => n.loop_guard,
        NotificationKind::LoadFailure => n.load_failures,
        NotificationKind::Download => n.downloads,
    }
}

/// Follows core events for the life of the app: keeps the tray current,
/// raises notifications, and applies the autostart setting.
pub fn start_updater(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let (mut rx, controller) = {
            let state = app.state::<HostState>();
            let core = state.core.read().await;
            let Some(c) = core.as_ref() else { return };
            (c.events.subscribe(), c.controller.clone())
        };
        apply_autostart(&app).await;
        if let Some(s) = controller.status().await {
            apply(&app, &s);
        }
        loop {
            match rx.recv().await {
                Ok(AppEvent::ModelState { .. } | AppEvent::Gate { .. }) | Err(RecvError::Lagged(_)) => {
                    if let Some(s) = controller.status().await {
                        apply(&app, &s);
                    }
                }
                Ok(AppEvent::ConfigReloaded) => apply_autostart(&app).await,
                Ok(AppEvent::Notification { kind, title, body }) => {
                    let allowed = {
                        let state = app.state::<HostState>();
                        let core = state.core.read().await;
                        core.as_ref().is_some_and(|c| enabled(c, kind))
                    };
                    if allowed {
                        notify(&app, &title, &body);
                    }
                }
                Ok(AppEvent::ConfigError { parse_error, issues }) => {
                    let detail = parse_error
                        .or_else(|| issues.first().map(|i| format!("{}: {}", i.path, i.message)))
                        .unwrap_or_default();
                    notify(&app, "config.json was not applied", &detail);
                }
                Ok(_) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// Makes the Windows autostart entry match `ui.start_with_windows` (FR-APP-2).
async fn apply_autostart(app: &AppHandle) {
    use tauri_plugin_autostart::ManagerExt;
    let want = {
        let state = app.state::<HostState>();
        let core = state.core.read().await;
        core.as_ref().is_some_and(|c| c.config().ui.start_with_windows)
    };
    let launcher = app.autolaunch();
    let current = launcher.is_enabled().unwrap_or(false);
    if want != current {
        let result = if want { launcher.enable() } else { launcher.disable() };
        if let Err(e) = result {
            warn!(error = %e, want, "could not update the start-with-Windows setting");
        }
    }
}
