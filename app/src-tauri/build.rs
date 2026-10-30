//! Build script: embeds the Tauri context and declares the app's commands as
//! an explicit allow-list (spec §17.6, §19: "the WebView can only call the
//! listed commands"). A command missing here cannot be invoked from the page,
//! even if it is registered in `main.rs`.

/// Every command the UI may call. Keep in sync with `main.rs` and
/// `capabilities/default.json`.
const COMMANDS: &[&str] = &[
    "get_status",
    "get_startup",
    "get_config",
    "save_config",
    "validate_config",
    "get_param_catalog",
    "refresh_catalog",
    "preview_launch",
    "load_model",
    "unload_model",
    "restart_model",
    "unload_all",
    "allow_anyway",
    "release_hold",
    "set_paused",
    "list_running_processes",
    "check_game_executable",
    "default_browse_dir",
    "list_gpu_adapters",
    "set_api_key",
    "secret_is_set",
    "set_secret",
    "clear_secret",
    "get_logs",
    "open_config_folder",
    "test_load",
    "check_engine_update",
    "check_model_updates",
    "install_engine",
    "engine_install_progress",
    "list_installed_engines",
    "switch_engine",
    "export_diagnostics",
    "hf_search",
    "hf_repo",
    "download_enqueue",
    "download_pause",
    "download_resume",
    "download_cancel",
    "download_dismiss",
    "download_list",
    "library_list",
    "library_delete",
    "add_downloaded_model",
    "add_downloaded_draft",
    "find_draft_models",
    "gguf_summary",
    "speculative_memory",
];

fn main() {
    let app = tauri_build::AppManifest::new().commands(COMMANDS);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(app)).expect("failed to run tauri-build");
}
