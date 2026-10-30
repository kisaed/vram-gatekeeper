//! Tauri commands: the only functions the settings page can call
//! (spec §17.5).
//!
//! Responsibility: thin wrappers that translate page requests into calls on
//! the core and results back into serialisable values. No logic lives here;
//! anything worth testing is in `gatekeeper-core`.
//! Not responsible for: windowing or the tray.
//!
//! The list of commands is also declared in `build.rs` and
//! `capabilities/default.json`; a command missing from either cannot be called.

use std::path::PathBuf;

use gatekeeper_core::app::{LaunchPreview, RunningApp, SECRET_LAN_API_KEY, StartupInfo};
use gatekeeper_core::config::{Config, ValidationIssue, schema::ModelConfig};
use gatekeeper_core::detect::game_list::{GameCheck, RunningApp as RunningProgram, check_executable, running_apps};
use gatekeeper_core::engine::updater::Backend;
use gatekeeper_core::gate::controller::StatusSnapshot;
use gatekeeper_core::gpu::AdapterInfo;
use gatekeeper_core::hf::api::{RepoSummary, SearchSort};
use gatekeeper_core::hf::draft_search::DraftSuggestion;
use gatekeeper_core::hf::library::LibraryFile;
use gatekeeper_core::hf::queue::{DownloadInfo, DownloadSpec};
use gatekeeper_core::hf::view::RepoView;
use gatekeeper_core::params::catalog::ParamCatalog;
use gatekeeper_core::speculative::{GgufSummary, summarize};
use gatekeeper_core::sync::MutexExt;
use tauri::State;
use tokio::sync::RwLockReadGuard;

use crate::state::{HostState, UiError};

/// Convenience: the running core under a read lock, or a clear error.
async fn core<'a>(state: &'a State<'_, HostState>) -> Result<RwLockReadGuard<'a, RunningApp>, UiError> {
    RwLockReadGuard::try_map(state.core.read().await, Option::as_ref).map_err(|_| UiError::not_running())
}

#[tauri::command]
pub async fn get_startup(state: State<'_, HostState>) -> Result<StartupInfo, UiError> {
    let startup_error = state.startup_error.lock_ok().clone();
    let guard = state.core.read().await;
    let info = guard.as_ref().map_or_else(
        || StartupInfo::not_running(startup_error.clone()),
        |core| StartupInfo::running(core, startup_error.clone()),
    );
    // The window shows the app's version, not the core library's.
    Ok(StartupInfo { version: env!("CARGO_PKG_VERSION"), ..info })
}

#[tauri::command]
pub async fn get_status(state: State<'_, HostState>) -> Result<StatusSnapshot, UiError> {
    let core = core(&state).await?;
    core.controller.status().await.ok_or_else(UiError::not_running)
}

#[tauri::command]
pub async fn get_config(state: State<'_, HostState>) -> Result<Config, UiError> {
    let core = core(&state).await?;
    Ok((*core.config()).clone())
}

#[tauri::command]
pub async fn save_config(state: State<'_, HostState>, config: Config) -> Result<(), UiError> {
    let core = core(&state).await?;
    tokio::task::block_in_place(|| core.save_config(config))
        .map_err(|issues| UiError::from_issues(issues, "The settings are not valid."))
}

#[tauri::command]
pub async fn validate_config(state: State<'_, HostState>, config: Config) -> Result<Vec<ValidationIssue>, UiError> {
    let core = core(&state).await?;
    Ok(tokio::task::block_in_place(|| core.validate_config(&config)))
}

#[tauri::command]
pub async fn get_param_catalog(state: State<'_, HostState>) -> Result<ParamCatalog, UiError> {
    let core = core(&state).await?;
    Ok(core.catalog())
}

#[tauri::command]
pub async fn refresh_catalog(state: State<'_, HostState>) -> Result<ParamCatalog, UiError> {
    let core = core(&state).await?;
    Ok(core.refresh_catalog().await?)
}

#[tauri::command]
pub async fn preview_launch(state: State<'_, HostState>, model: ModelConfig) -> Result<LaunchPreview, UiError> {
    let core = core(&state).await?;
    Ok(core.preview_launch(&model))
}

#[tauri::command]
pub async fn load_model(state: State<'_, HostState>, id: String) -> Result<(), UiError> {
    let core = core(&state).await?;
    core.controller.load(&id).await.map_err(|r| UiError::msg(r.message))
}

#[tauri::command]
pub async fn unload_model(state: State<'_, HostState>, id: String, hold: bool) -> Result<(), UiError> {
    let core = core(&state).await?;
    core.controller.unload(&id, hold).await;
    Ok(())
}

#[tauri::command]
pub async fn restart_model(state: State<'_, HostState>, id: String) -> Result<(), UiError> {
    let core = core(&state).await?;
    core.controller.restart(&id).await;
    Ok(())
}

#[tauri::command]
pub async fn unload_all(state: State<'_, HostState>) -> Result<(), UiError> {
    let core = core(&state).await?;
    core.controller.unload_all();
    Ok(())
}

#[tauri::command]
pub async fn allow_anyway(state: State<'_, HostState>) -> Result<(), UiError> {
    let core = core(&state).await?;
    core.controller.allow_anyway();
    Ok(())
}

#[tauri::command]
pub async fn release_hold(state: State<'_, HostState>, id: String) -> Result<(), UiError> {
    let core = core(&state).await?;
    core.controller.release_hold(&id);
    Ok(())
}

#[tauri::command]
pub async fn set_paused(state: State<'_, HostState>, paused: bool) -> Result<(), UiError> {
    let core = core(&state).await?;
    core.controller.set_paused(paused);
    Ok(())
}

#[tauri::command]
pub async fn list_running_processes(state: State<'_, HostState>) -> Result<Vec<RunningProgram>, UiError> {
    let core = core(&state).await?;
    let vram = core.controller.status().await.and_then(|s| s.vram);
    // Enumerating processes and windows is quick but synchronous Win32 work.
    tauri::async_runtime::spawn_blocking(move || running_apps(vram.as_ref()))
        .await
        .map_err(|e| UiError::msg(e.to_string()))
}

#[tauri::command]
pub async fn check_game_executable(state: State<'_, HostState>, input: String) -> Result<GameCheck, UiError> {
    let core = core(&state).await?;
    let cfg = core.config();
    Ok(check_executable(&input, &cfg.games.executables))
}

#[tauri::command]
pub fn default_browse_dir() -> String {
    gatekeeper_core::detect::game_list::default_browse_dir()
}

#[tauri::command]
pub async fn list_gpu_adapters() -> Result<Vec<AdapterInfo>, UiError> {
    Ok(gatekeeper_core::gpu::windows_monitor::list_adapters()?)
}

/// Sets (or, with `None`, clears) the LAN API key and records the flag in
/// `config.json`.
#[tauri::command]
pub async fn set_api_key(state: State<'_, HostState>, key: Option<String>) -> Result<(), UiError> {
    let core = core(&state).await?;
    core.set_api_key(key.as_deref())?;
    core.set_api_key_flag(key.as_deref().is_some_and(|k| !k.is_empty()))?;
    Ok(())
}

/// Reports whether a named secret exists, without revealing it.
#[tauri::command]
pub async fn secret_is_set(state: State<'_, HostState>, name: String) -> Result<bool, UiError> {
    let core = core(&state).await?;
    Ok(core.secrets().get(&secret_name(&name)?).is_some())
}

#[tauri::command]
pub async fn set_secret(state: State<'_, HostState>, name: String, value: String) -> Result<(), UiError> {
    let core = core(&state).await?;
    core.secrets().set(&secret_name(&name)?, &value).map_err(UiError::msg)
}

#[tauri::command]
pub async fn clear_secret(state: State<'_, HostState>, name: String) -> Result<(), UiError> {
    let core = core(&state).await?;
    core.secrets().clear(&secret_name(&name)?);
    Ok(())
}

// ---------------------------------------------------------------- Hugging Face

#[tauri::command]
pub async fn hf_search(
    state: State<'_, HostState>,
    query: String,
    sort: SearchSort,
) -> Result<Vec<RepoSummary>, UiError> {
    let core = core(&state).await?;
    core.hf.search(&query, sort, 30).await.map_err(|e| UiError::msg(e.to_string()))
}

#[tauri::command]
pub async fn hf_repo(state: State<'_, HostState>, repo: String, revision: Option<String>) -> Result<RepoView, UiError> {
    let core = core(&state).await?;
    Ok(core.repo_view(&repo, revision.as_deref().unwrap_or("main")).await?)
}

#[tauri::command]
pub async fn download_enqueue(state: State<'_, HostState>, request: DownloadSpec) -> Result<u64, UiError> {
    let core = core(&state).await?;
    core.downloads.enqueue(request).await.map_err(UiError::msg)
}

#[tauri::command]
pub async fn download_pause(state: State<'_, HostState>, id: u64) -> Result<(), UiError> {
    let core = core(&state).await?;
    core.downloads.pause(id);
    Ok(())
}

#[tauri::command]
pub async fn download_resume(state: State<'_, HostState>, id: u64) -> Result<(), UiError> {
    let core = core(&state).await?;
    core.downloads.resume(id);
    Ok(())
}

#[tauri::command]
pub async fn download_cancel(state: State<'_, HostState>, id: u64) -> Result<(), UiError> {
    let core = core(&state).await?;
    core.downloads.cancel(id);
    Ok(())
}

#[tauri::command]
pub async fn download_dismiss(state: State<'_, HostState>, id: u64) -> Result<(), UiError> {
    let core = core(&state).await?;
    core.downloads.dismiss(id);
    Ok(())
}

#[tauri::command]
pub async fn download_list(state: State<'_, HostState>) -> Result<Vec<DownloadInfo>, UiError> {
    let core = core(&state).await?;
    Ok(core.downloads.list().await)
}

#[tauri::command]
pub async fn library_list(state: State<'_, HostState>) -> Result<Vec<LibraryFile>, UiError> {
    let core = core(&state).await?;
    // Scanning reads GGUF-free metadata only, but touches the disk.
    Ok(tokio::task::block_in_place(|| core.library()))
}

#[tauri::command]
pub async fn library_delete(state: State<'_, HostState>, path: String) -> Result<usize, UiError> {
    let core = core(&state).await?;
    // Refuse while any model using the file is running; the core refuses files that models reference at all.
    core.delete_library_file(std::path::Path::new(&path)).map_err(UiError::msg)
}

/// Creates a model entry from a finished download and returns its id.
#[tauri::command]
pub async fn add_downloaded_model(state: State<'_, HostState>, download_id: u64) -> Result<String, UiError> {
    let core = core(&state).await?;
    let info = core
        .downloads
        .list()
        .await
        .into_iter()
        .find(|d| d.id == download_id && d.paths.len() == d.files.len() && !d.paths.is_empty())
        .ok_or_else(|| UiError::msg("That download has not finished."))?;
    core.add_downloaded_model(&info).map_err(|issues| UiError::from_issues(issues, "The model could not be added."))
}

/// Attaches a finished download to an existing model as its draft file
/// (spec §10.6 "Add as a Draft Model").
#[tauri::command]
pub async fn add_downloaded_draft(
    state: State<'_, HostState>,
    download_id: u64,
    model_id: String,
) -> Result<(), UiError> {
    let core = core(&state).await?;
    let info = core
        .downloads
        .list()
        .await
        .into_iter()
        .find(|d| d.id == download_id && d.paths.len() == d.files.len() && !d.paths.is_empty())
        .ok_or_else(|| UiError::msg("That download has not finished."))?;
    core.add_downloaded_draft(&info, &model_id)
        .map_err(|issues| UiError::from_issues(issues, "The draft file could not be attached."))
}

/// Suggests draft models of the same family as `repo` (spec §10.6).
#[tauri::command]
pub async fn find_draft_models(state: State<'_, HostState>, repo: String) -> Result<Vec<DraftSuggestion>, UiError> {
    let core = core(&state).await?;
    core.find_draft_models(&repo).await.map_err(|e| UiError::msg(e.to_string()))
}

/// What the Speculative Decoding tab shows about a GGUF file (spec §10.6).
#[tauri::command]
pub async fn gguf_summary(path: String) -> Result<GgufSummary, UiError> {
    // Reads the file header, so keep it off the async threads.
    tokio::task::block_in_place(|| summarize(std::path::Path::new(&path))).map_err(|e| UiError::msg(e.to_string()))
}

/// Extra GPU memory in MiB that the model's speculative setup is estimated to
/// need (spec §10.6), for the Speculative Decoding tab.
#[tauri::command]
pub async fn speculative_memory(model: ModelConfig) -> Result<u64, UiError> {
    Ok(tokio::task::block_in_place(|| gatekeeper_core::supervisor::footprint::draft_extra_mib(&model)))
}

/// Only these secret names may be touched from the page.
fn secret_name(name: &str) -> Result<String, UiError> {
    match name {
        "hf_token" | SECRET_LAN_API_KEY => Ok(name.to_owned()),
        other => Err(UiError::msg(format!("unknown secret '{other}'"))),
    }
}

/// Last lines of a log. `source` is `app` or `model:<id>`.
#[tauri::command]
pub async fn get_logs(state: State<'_, HostState>, source: String, tail: usize) -> Result<Vec<String>, UiError> {
    let core = core(&state).await?;
    let tail = tail.clamp(1, 5000);
    if let Some(id) = source.strip_prefix("model:") {
        return Ok(core.logs.tail(id, tail));
    }
    let dir = core.paths().data_dir.join("logs");
    Ok(tauri::async_runtime::spawn_blocking(move || read_latest_app_log(&dir, tail)).await.unwrap_or_default())
}

/// Reads the last `n` lines of the newest `app*.log`.
fn read_latest_app_log(dir: &std::path::Path, n: usize) -> Vec<String> {
    let newest: Option<PathBuf> = std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("app"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path());
    let Some(path) = newest else { return Vec::new() };
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|l| (*l).to_owned()).collect()
}

/// Opens the folder containing `config.json` in Explorer.
#[tauri::command]
pub async fn open_config_folder(state: State<'_, HostState>) -> Result<(), UiError> {
    let core = core(&state).await?;
    let dir = core.paths().config_dir.clone();
    std::process::Command::new("explorer").arg(dir).spawn().map(|_| ()).map_err(|e| UiError::msg(e.to_string()))
}

/// Checks downloaded models for newer versions on Hugging Face (only when asked).
#[tauri::command]
pub async fn check_model_updates(
    state: State<'_, HostState>,
    model_id: Option<String>,
) -> Result<Vec<gatekeeper_core::hf::update::ModelUpdate>, UiError> {
    let core = core(&state).await?;
    Ok(core.check_model_updates(model_id.as_deref()).await)
}

/// Looks for a newer llama.cpp build (only when the user asks).
#[tauri::command]
pub async fn check_engine_update(
    state: State<'_, HostState>,
    backend: Option<Backend>,
) -> Result<gatekeeper_core::engine::updater::UpdateCheck, UiError> {
    let core = core(&state).await?;
    Ok(core.check_engine_update(backend).await?)
}

/// Starts downloading an engine build in the background. The core picks the
/// files itself from the last update check; the page only names the build.
#[tauri::command]
pub async fn install_engine(state: State<'_, HostState>, tag: String, backend: Backend) -> Result<(), UiError> {
    let core = core(&state).await?;
    Ok(core.start_engine_install(&tag, backend)?)
}

/// Progress of the engine download.
#[tauri::command]
pub async fn engine_install_progress(
    state: State<'_, HostState>,
) -> Result<gatekeeper_core::engine::updater::InstallProgress, UiError> {
    let core = core(&state).await?;
    Ok(core.engine_install_progress())
}

/// Engines this app downloaded earlier.
#[tauri::command]
pub async fn list_installed_engines(
    state: State<'_, HostState>,
) -> Result<Vec<gatekeeper_core::engine::updater::InstalledEngine>, UiError> {
    let core = core(&state).await?;
    Ok(core.installed_engines())
}

/// Switches to a downloaded engine, or back to the bundled one with `None`.
#[tauri::command]
pub async fn switch_engine(
    state: State<'_, HostState>,
    dir: Option<String>,
) -> Result<gatekeeper_core::app::EngineSwitchReport, UiError> {
    let core = core(&state).await?;
    // Only folders this app downloaded can be selected from the window; a
    // custom folder goes through the settings form, which validates it.
    let dir = match dir {
        None => None,
        Some(d) => {
            let wanted = PathBuf::from(&d);
            let known = core.installed_engines().into_iter().any(|e| std::path::Path::new(&e.path) == wanted);
            if !known {
                return Err(UiError::msg("That engine was not downloaded by VRAM Gatekeeper."));
            }
            Some(wanted)
        }
    };
    Ok(core.switch_engine(dir).await?)
}

/// Starts a model with its saved settings, reports how it loaded, and unloads
/// it again (spec §10.5 item 8).
#[tauri::command]
pub async fn test_load(
    state: State<'_, HostState>,
    id: String,
) -> Result<gatekeeper_core::diagnostics::TestLoadReport, UiError> {
    let core = core(&state).await?;
    Ok(core.test_load(&id).await)
}

/// Writes the diagnostics bundle into the data folder, shows it in Explorer and
/// returns its path. Nothing is sent anywhere.
#[tauri::command]
pub async fn export_diagnostics(state: State<'_, HostState>) -> Result<String, UiError> {
    let core = core(&state).await?;
    let stamp = gatekeeper_core::supervisor::log_files::today();
    let dest = core.paths().data_dir.join("diagnostics").join(format!("vram-gatekeeper-diagnostics-{stamp}.zip"));
    core.export_diagnostics(&dest).await?;
    // `/select,` highlights the file; Explorer wants backslashes.
    let arg = format!("/select,{}", dest.display().to_string().replace('/', "\\"));
    let _ = std::process::Command::new("explorer").arg(arg).spawn();
    Ok(dest.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_secrets_can_be_touched() {
        assert!(secret_name("hf_token").is_ok());
        assert!(secret_name("lan_api_key").is_ok());
        assert!(secret_name("something_else").is_err());
    }
}

/// The page can call only commands that appear in three places (spec §17.6, §19):
/// the handler list in `main.rs`, the allow-list in `build.rs`, and the
/// capability file. A command missing from one of them fails at run time with
/// "not allowed", which no other test notices.
#[cfg(test)]
mod allow_list_tests {
    const COMMANDS_RS: &str = include_str!("commands.rs");
    const MAIN_RS: &str = include_str!("main.rs");
    const BUILD_RS: &str = include_str!("../build.rs");
    const CAPABILITY: &str = include_str!("../capabilities/default.json");

    /// Names of the functions marked `#[tauri::command]`.
    fn defined_commands() -> Vec<String> {
        let mut names = Vec::new();
        let mut lines = COMMANDS_RS.lines();
        while let Some(line) = lines.next() {
            if line.trim() != "#[tauri::command]" {
                continue;
            }
            let signature = lines.by_ref().find(|l| l.contains("fn ")).expect("a function follows the attribute");
            let after_fn = signature.split("fn ").nth(1).expect("fn keyword");
            names.push(after_fn.split(['(', '<']).next().expect("name").trim().to_owned());
        }
        names
    }

    #[test]
    fn every_command_is_registered_allowed_and_permitted() {
        let names = defined_commands();
        assert!(names.len() > 40, "the parser finds the commands: {names:?}");
        for name in &names {
            assert!(MAIN_RS.contains(&format!("commands::{name},")), "{name} is not in the handler list in main.rs");
            assert!(BUILD_RS.contains(&format!("\"{name}\",")), "{name} is not in COMMANDS in build.rs");
            let permission = format!("\"allow-{}\"", name.replace('_', "-"));
            assert!(CAPABILITY.contains(&permission), "{permission} is not in capabilities/default.json");
        }
    }
}
