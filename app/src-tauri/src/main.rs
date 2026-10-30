//! VRAM Gatekeeper: Tauri host (spec §5, §17, decision D2).
//!
//! One process holds everything: the core runs on a small tokio runtime from
//! launch, the tray icon is native, and the settings window (a WebView) is
//! created only when opened and destroyed when closed, so an idle app owns no
//! WebView2 processes (NFR-1).

// Hide the console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod core_boot;
mod state;
mod tray;
mod tray_icons;
mod tray_model;
mod ui_bridge;
mod window;

use tauri::RunEvent;
use tauri_plugin_autostart::MacosLauncher;

/// Worker threads for the async runtime. The load is I/O bound and tiny
/// (spec §5.2), so two are plenty and keep idle memory down.
const RUNTIME_WORKERS: usize = 2;

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(RUNTIME_WORKERS)
        .enable_all()
        .build()
        .expect("the async runtime must start");
    tauri::async_runtime::set(runtime.handle().clone());

    let app = tauri::Builder::default()
        // Launching a second copy opens the existing window (spec §17.1).
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| window::open(app)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(state::HostState::default())
        .setup(|app| {
            tray::create(app.handle())?;
            tauri::async_runtime::spawn(core_boot::start(app.handle().clone()));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_startup,
            commands::get_config,
            commands::save_config,
            commands::validate_config,
            commands::get_param_catalog,
            commands::refresh_catalog,
            commands::preview_launch,
            commands::load_model,
            commands::unload_model,
            commands::restart_model,
            commands::unload_all,
            commands::allow_anyway,
            commands::release_hold,
            commands::set_paused,
            commands::list_running_processes,
            commands::check_game_executable,
            commands::default_browse_dir,
            commands::list_gpu_adapters,
            commands::set_api_key,
            commands::secret_is_set,
            commands::set_secret,
            commands::clear_secret,
            commands::get_logs,
            commands::open_config_folder,
            commands::test_load,
            commands::check_engine_update,
            commands::check_model_updates,
            commands::install_engine,
            commands::engine_install_progress,
            commands::list_installed_engines,
            commands::switch_engine,
            commands::export_diagnostics,
            commands::hf_search,
            commands::hf_repo,
            commands::download_enqueue,
            commands::download_pause,
            commands::download_resume,
            commands::download_cancel,
            commands::download_dismiss,
            commands::download_list,
            commands::library_list,
            commands::library_delete,
            commands::add_downloaded_model,
            commands::add_downloaded_draft,
            commands::find_draft_models,
            commands::gguf_summary,
            commands::speculative_memory,
        ])
        .build(tauri::generate_context!())
        .expect("error while building the Tauri application");

    app.run(|handle, event| match event {
        // Closing the settings window must not quit the app: it lives in the
        // tray. Only an explicit exit (code Some) goes through.
        RunEvent::ExitRequested { code: None, api, .. } => api.prevent_exit(),
        RunEvent::Exit => {
            // Kill the llama-server children and stop the proxy before leaving.
            let state = tauri::Manager::state::<state::HostState>(handle);
            tauri::async_runtime::block_on(async {
                if let Some(core) = state.core.write().await.take() {
                    core.shutdown().await;
                }
            });
        }
        _ => {}
    });
    drop(runtime);
}
