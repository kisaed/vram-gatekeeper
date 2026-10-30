//! Starts the gatekeeper core inside the Tauri process (spec §5, D2).
//!
//! Responsibility: locate the bundled engine, set up file logging, start
//! [`RunningApp`], and park it in [`HostState`] (or record why it failed).
//! Not responsible for: tray or window behaviour.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use gatekeeper_core::app::{AppPaths, RunningApp, Services};
use gatekeeper_core::engine::bundled::pick_bundled_dir;
use gatekeeper_core::gpu::select_adapter;
use gatekeeper_core::sync::MutexExt;
use tauri::{AppHandle, Manager};
use tracing::{error, info};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

use crate::state::HostState;
use crate::tray;
use crate::ui_bridge;

/// Keeps the log writer thread alive for the process lifetime.
static LOG_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

/// Overrides where bundled engines are looked up (development builds, which
/// have no installed `resources` folder).
const ENV_BUNDLED_ENGINES: &str = "GATEKEEPER_BUNDLED_ENGINES";

/// Starts the core and then the tray updater. Runs once at launch.
pub async fn start(app: AppHandle) {
    let result = boot(&app).await;
    let state = app.state::<HostState>();
    match result {
        Ok(core) => {
            if !core.startup.issues.is_empty() || core.startup.parse_error.is_some() {
                tray::notify(
                    &app,
                    "config.json was not applied",
                    "The file has problems, so the last good settings are in use. Open the window to see them.",
                );
            }
            *state.core.write().await = Some(core);
            tray::start_updater(&app);
            ui_bridge::core_settled(&app);
        }
        Err(e) => {
            error!(error = %e, "the core could not start");
            *state.startup_error.lock_ok() = Some(e.to_string());
            tray::notify(&app, "VRAM Gatekeeper could not start", &e.to_string());
            ui_bridge::core_settled(&app);
        }
    }
}

/// Builds the paths and starts [`RunningApp`].
async fn boot(app: &AppHandle) -> gatekeeper_core::CoreResult<RunningApp> {
    let probe = AppPaths::windows_default(None)?;
    init_logging(&probe.data_dir.join("logs"));

    let root = bundled_root(app);
    let adapters = gatekeeper_core::gpu::windows_monitor::list_adapters().unwrap_or_default();
    let bundled = root.as_deref().and_then(|r| pick_bundled_dir(r, select_adapter(&adapters, None)));
    match &bundled {
        Some(dir) => info!(dir = %dir.display(), "using the bundled llama.cpp engine"),
        None => info!("no bundled llama.cpp engine found; the engine folder must be set in settings"),
    }

    let paths = AppPaths { bundled_engine_dir: bundled, ..probe };
    RunningApp::start(
        paths,
        Services { auto_platform: true, adapters: (!adapters.is_empty()).then_some(adapters), ..Services::default() },
    )
    .await
}

/// Folder holding the per-backend engine folders.
fn bundled_root(app: &AppHandle) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(ENV_BUNDLED_ENGINES) {
        return Some(PathBuf::from(dir));
    }
    app.path().resource_dir().ok().map(|r| r.join("engine")).filter(|p| p.is_dir())
}

/// Sends logs to a daily rolling file under `logs/` (spec §20).
fn init_logging(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let appender = match tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("app")
        .filename_suffix("log")
        .build(dir)
    {
        Ok(a) => a,
        Err(_) => return, // logging is best effort
    };
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let _ = LOG_GUARD.set(guard);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_writer(writer).with_ansi(false).with_env_filter(filter).try_init();
    // Old app logs share the retention rule of model logs.
    let _ = gatekeeper_core::supervisor::log_files::prune(dir, 7);
}
