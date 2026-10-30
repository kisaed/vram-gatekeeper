//! Wires the components together: start, reload, stop (spec §5).
//!
//! Responsibility: the composition root. It loads the config, builds the
//! controller, proxy, detectors and GPU sampler, keeps them in sync with
//! config changes, and shuts everything down cleanly. Both the Tauri host
//! and the headless CLI call [`RunningApp::start`].
//! Not responsible for: any decision logic (that lives in the components).
//!
//! Implements FR-APP-1, FR-MOD-3, NFR-5.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::defaults::{DEFAULT_HOST, DEFAULT_LOAD_TIMEOUT_SECONDS};
use crate::config::schema::Config;
use crate::config::store::{self, ConfigPaths, ConfigWatcher, LoadOutcome};
use crate::config::validate::{ValidateOptions, ValidationIssue, errors_of, is_loopback_host, validate};
use crate::detect::{GameDetector, GameSignal};
use crate::diagnostics::{self, TestLoadReport};
use crate::engine::install::{generate_catalog, resolve_engine_dir, validate_engine};
use crate::engine::updater::{self, Backend, InstallProgress, InstalledEngine, Release, UpdateCheck};
use crate::error::{CoreError, CoreResult};
use crate::events::{AppEvent, EventBus};
use crate::gate::controller::{self, ControllerHandle, Deps, ModelStatus};
use crate::gate::model_state::ModelStateKind;
use crate::gpu::{GpuMonitor, sampler};
use crate::hf::api::{HUB_URL, HfApi};
use crate::hf::queue::DownloadManager;
use crate::params::catalog::ParamCatalog;
use crate::ports::{HealthProbe, MemorySecretStore, ProcessLauncher, SecretStore, StateStore};
use crate::proxy::server::{self, ProxyState};
use crate::state_store::FileStateStore;
use crate::supervisor::health::HttpHealthProbe;
use crate::supervisor::launcher::{LauncherContext, RealLauncher};
use crate::supervisor::log_capture::LogStore;
use crate::supervisor::log_files;
use crate::sync::{MutexExt, RwLockExt};

/// Name of the secret holding the LAN API key (spec D7).
pub const SECRET_LAN_API_KEY: &str = "lan_api_key";

/// Longest pause between checks while waiting for a model to settle.
const WAIT_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Folder and file locations (spec §14.1).
#[derive(Debug, Clone)]
pub struct AppPaths {
    /// Holds `config.json` (roaming; `%APPDATA%\VRAM Gatekeeper`).
    pub config_dir: PathBuf,
    /// Holds state, logs and downloaded engines (`%LOCALAPPDATA%\VRAM Gatekeeper`).
    pub data_dir: PathBuf,
    /// Engine shipped inside the installer, if any (spec §16.1).
    pub bundled_engine_dir: Option<PathBuf>,
}

impl AppPaths {
    /// Standard Windows locations, unless overridden.
    ///
    /// `GATEKEEPER_CONFIG_DIR` and `GATEKEEPER_DATA_DIR` replace the config and
    /// data folders; useful for a portable install, tests, and trying the app
    /// without touching real settings.
    ///
    /// # Errors
    /// Returns an error if a folder is not overridden and `%APPDATA%` or
    /// `%LOCALAPPDATA%` is not set.
    pub fn windows_default(bundled_engine_dir: Option<PathBuf>) -> CoreResult<Self> {
        let var = |name: &str| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .ok_or_else(|| CoreError::Other(format!("environment variable {name} is not set")))
        };
        let config_dir = match std::env::var_os("GATEKEEPER_CONFIG_DIR") {
            Some(d) => PathBuf::from(d),
            None => var("APPDATA")?.join("VRAM Gatekeeper"),
        };
        let data_dir = match std::env::var_os("GATEKEEPER_DATA_DIR") {
            Some(d) => PathBuf::from(d),
            None => var("LOCALAPPDATA")?.join("VRAM Gatekeeper"),
        };
        Ok(Self { config_dir, data_dir, bundled_engine_dir })
    }

    /// Folder for per-model log files.
    pub fn model_logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs").join("models")
    }

    /// Path of `state.json`.
    pub fn state_file(&self) -> PathBuf {
        self.data_dir.join("state.json")
    }

    /// Folder where downloaded engines and cached parameter catalogs live
    /// (never the install directory).
    pub fn engines_dir(&self) -> PathBuf {
        self.data_dir.join("engines")
    }
}

/// Replaceable collaborators; tests and the host substitute their own.
#[derive(Default)]
pub struct Services {
    /// Process launcher; defaults to [`RealLauncher`].
    pub launcher: Option<Arc<dyn ProcessLauncher>>,
    /// Health probe; defaults to [`HttpHealthProbe`].
    pub probe: Option<Arc<dyn HealthProbe>>,
    /// Learned-data store; defaults to a file in `data_dir`.
    pub state: Option<Arc<dyn StateStore>>,
    /// Secret storage; defaults to in-memory.
    pub secrets: Option<Arc<dyn SecretStore>>,
    /// GPU monitor; `None` disables VRAM rules.
    pub gpu: Option<Arc<dyn GpuMonitor>>,
    /// Adapters the host already listed, so `auto_platform` does not list them
    /// again; `None` lists them on demand.
    pub adapters: Option<Vec<crate::gpu::AdapterInfo>>,
    /// Game detectors to run.
    pub detectors: Vec<Box<dyn GameDetector>>,
    /// Signalled when the PC wakes from sleep, to make scanning detectors look
    /// again at once (the Windows build fills this in with `auto_platform`).
    pub rescan_triggers: Vec<Arc<tokio::sync::Notify>>,
    /// Hugging Face base URL; `None` means the real Hub (tests point it at a stub).
    pub hf_base_url: Option<String>,
    /// On Windows, fill in any service left unset above with the real one
    /// (Credential Manager, PDH GPU monitor, process/foreground/Steam
    /// detectors). Tests leave this off and supply fakes.
    pub auto_platform: bool,
}

/// Shared pieces, cloned into background tasks.
struct Inner {
    /// Progress of the engine download in flight, if any.
    engine_install: updater::ProgressCell,
    /// The release list from the last update check; installs are planned from it.
    engine_releases: Mutex<Vec<Release>>,
    /// Locations.
    paths: AppPaths,
    /// Config publisher; receivers see every applied config.
    config_tx: watch::Sender<Arc<Config>>,
    /// Gate controller.
    controller: ControllerHandle,
    /// Event bus.
    events: EventBus,
    /// Secrets.
    secrets: Arc<dyn SecretStore>,
    /// API key slot read by the proxy.
    api_key: Arc<RwLock<Option<String>>>,
    /// Current parameter catalog.
    catalog: Arc<RwLock<ParamCatalog>>,
}

/// A started application. Dropping it without calling [`RunningApp::shutdown`]
/// still ends the process tree, because children live in a kill-on-close job.
pub struct RunningApp {
    /// Shared pieces.
    inner: Arc<Inner>,
    /// Gate controller front door.
    pub controller: ControllerHandle,
    /// Event bus.
    pub events: EventBus,
    /// Model log ring buffers.
    pub logs: LogStore,
    /// Hugging Face client (search, listings).
    pub hf: HfApi,
    /// The download queue.
    pub downloads: DownloadManager,
    /// Address the proxy listens on.
    pub proxy_addr: SocketAddr,
    /// True when the proxy accepts connections from the network.
    pub lan_bound: bool,
    /// Result of loading the config at startup.
    pub startup: StartupReport,
    /// Why VRAM features are off, if they are (no counters, integrated GPU).
    pub gpu_notice: Option<String>,
    /// Stops background tasks.
    shutdown: CancellationToken,
    /// Background tasks, awaited at shutdown.
    tasks: Vec<JoinHandle<()>>,
    /// Keeps the config file watcher alive.
    _watcher: std::sync::Mutex<Option<ConfigWatcher>>,
    /// Detectors, stopped at shutdown.
    detectors: std::sync::Mutex<Vec<Box<dyn GameDetector>>>,
    /// Keeps the sleep/resume registration alive.
    #[cfg(windows)]
    _power: std::sync::Mutex<Option<crate::platform::windows::power::ResumeWatcher>>,
}

/// What happened while loading the config at startup, for the UI to show.
#[derive(Debug, Clone)]
pub struct StartupReport {
    /// Where the config came from.
    pub source: store::LoadSource,
    /// Problems with the rejected file.
    pub issues: Vec<ValidationIssue>,
    /// Parse failure of the rejected file.
    pub parse_error: Option<String>,
}

/// Facts the Status page shows once at startup.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StartupInfo {
    /// App version.
    pub version: &'static str,
    /// Where the active config came from (`Primary`, `Backup`, `Defaults`, `Created`).
    pub config_source: String,
    /// Validation problems of a rejected `config.json`.
    pub issues: Vec<ValidationIssue>,
    /// Parse failure of a rejected `config.json`.
    pub parse_error: Option<String>,
    /// Why VRAM features are off, if they are.
    pub gpu_notice: Option<String>,
    /// Address the proxy listens on.
    pub proxy_addr: Option<String>,
    /// True if the proxy accepts network connections.
    pub lan_bound: bool,
    /// Why the core failed to start.
    pub startup_error: Option<String>,
    /// Full path of `config.json`.
    pub config_path: Option<String>,
}

impl StartupInfo {
    /// The facts of a running core; `startup_error` is the host's record of an earlier failure.
    pub fn running(app: &RunningApp, startup_error: Option<String>) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            config_source: format!("{:?}", app.startup.source),
            issues: app.startup.issues.clone(),
            parse_error: app.startup.parse_error.clone(),
            gpu_notice: app.gpu_notice.clone(),
            proxy_addr: Some(app.proxy_addr.to_string()),
            lan_bound: app.lan_bound,
            startup_error,
            config_path: Some(app.paths().config_dir.join("config.json").to_string_lossy().into_owned()),
        }
    }

    /// What is known before the core is up, or after it failed to start.
    pub fn not_running(startup_error: Option<String>) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            config_source: String::new(),
            issues: Vec::new(),
            parse_error: None,
            gpu_notice: None,
            proxy_addr: None,
            lan_bound: false,
            startup_error,
            config_path: None,
        }
    }
}

/// The command line a model would be launched with (spec §10.5, FR-MOD-5).
#[derive(Debug, Clone, serde::Serialize)]
pub struct LaunchPreview {
    /// Full command line; the API key is masked and the port is a placeholder.
    pub command: String,
    /// Settings that were skipped and why.
    pub warnings: Vec<crate::params::ArgvWarning>,
    /// Speculative decoding findings for this model (spec §10.6).
    pub findings: Vec<ValidationIssue>,
}

/// What switching engines changed (spec §16.2).
#[derive(Debug, Clone, serde::Serialize)]
pub struct EngineSwitchReport {
    /// Version line of the engine now in use.
    pub engine_version: String,
    /// Number of parameters the new engine offers.
    pub param_count: usize,
    /// Model settings the new engine no longer understands, as `(model id, key, why)`.
    pub unsupported: Vec<(String, String, String)>,
}

/// The adapter the user's GPU settings point at (or the best one).
#[cfg(windows)]
fn selected_adapter(cfg: &Config) -> Option<crate::gpu::AdapterInfo> {
    let list = crate::gpu::windows_monitor::list_adapters().ok()?;
    crate::gpu::select_adapter(&list, cfg.engine.gpu_adapter_luid.as_deref()).cloned()
}

/// Non-Windows builds have no adapter list.
#[cfg(not(windows))]
fn selected_adapter(_cfg: &Config) -> Option<crate::gpu::AdapterInfo> {
    None
}

/// The load timeout of model `id`, or the default when it is unknown.
fn cfg_load_timeout(cfg: &Config, id: &str) -> std::time::Duration {
    let seconds = cfg
        .models
        .iter()
        .find(|m| m.id == id)
        .map_or(DEFAULT_LOAD_TIMEOUT_SECONDS, |m| m.lifecycle.load_timeout_seconds);
    std::time::Duration::from_secs(u64::from(seconds))
}

impl RunningApp {
    /// Starts everything.
    ///
    /// # Errors
    /// Fails if the config directory is unusable, the job object cannot be
    /// created, or the proxy port cannot be bound.
    pub async fn start(paths: AppPaths, services: Services) -> CoreResult<Self> {
        let LoadOutcome { config, issues, parse_error, source, migrated_from } =
            store::load_or_init(&ConfigPaths::new(&paths.config_dir))?;
        if let Some(from) = migrated_from {
            info!(from, "config migrated in memory; it will be rewritten on the next save");
        }
        if let Err(e) = store::write_schema(&ConfigPaths::new(&paths.config_dir)) {
            warn!(error = %e, "could not write config.schema.json");
        }
        let mut startup = StartupReport { source, issues, parse_error };
        let mut services = services;
        let auto_platform = services.auto_platform;
        let gpu_notice = if services.auto_platform { fill_platform_services(&config, &mut services) } else { None };
        let config = Arc::new(config);
        let (config_tx, config_rx) = watch::channel(config.clone());

        let events = EventBus::new();
        let logs = LogStore::new();
        let secrets = services.secrets.unwrap_or_else(|| Arc::new(MemorySecretStore::default()));
        let api_key = Arc::new(RwLock::new(secrets.get(SECRET_LAN_API_KEY)));

        // Fail safe (spec §19): the validator trusts `api_key_set`, which anyone
        // can hand-edit, so the *actual* stored key decides whether the proxy may
        // listen beyond this machine.
        let has_key = api_key.read_ok().is_some();
        let mut bind_config = (*config).clone();
        if (!is_loopback_host(&bind_config.server.host) || bind_config.server.allow_lan) && !has_key {
            warn!("network access was requested but no API key is stored; listening on localhost only");
            startup.issues.push(ValidationIssue::new(
                "server.allow_lan",
                "Network access needs an API key, and none is stored, so the proxy is listening on localhost only. Set a key, then restart.",
            ));
            bind_config.server.allow_lan = false;
            bind_config.server.host = DEFAULT_HOST.to_owned();
        }
        let log_dir = paths.model_logs_dir();
        let prune_dir = log_dir.clone();
        let retention = config.logging.retention_days;
        // Loading the catalog may run the engine; pruning old logs is independent of it.
        let (catalog, _) = tokio::join!(
            load_catalog(&paths, &config),
            tokio::task::spawn_blocking(move || log_files::prune(&prune_dir, retention)),
        );
        let catalog = Arc::new(RwLock::new(catalog));

        let launcher: Arc<dyn ProcessLauncher> = match services.launcher {
            Some(l) => l,
            None => Arc::new(RealLauncher::new(LauncherContext {
                config: config_rx.clone(),
                bundled_engine_dir: paths.bundled_engine_dir.clone(),
                catalog: catalog.clone(),
                log_dir: Some(log_dir),
            })?),
        };
        let probe = services.probe.unwrap_or_else(|| Arc::new(HttpHealthProbe::new()));
        let state = services.state.unwrap_or_else(|| Arc::new(FileStateStore::open(paths.state_file())));

        let (controller, controller_task) = controller::spawn(
            config.clone(),
            Deps { launcher, probe, state, events: events.clone(), logs: logs.clone() },
        );

        let shutdown = CancellationToken::new();
        let mut tasks = vec![controller_task];

        // Proxy.
        let listener = server::bind(&bind_config)
            .await
            .map_err(|e| CoreError::io(format!("{}:{}", bind_config.server.host, bind_config.server.port), e))?;
        let proxy_addr = listener.local_addr().map_err(|e| CoreError::io("proxy listener", e))?;
        let lan_bound = !proxy_addr.ip().is_loopback();
        let mut state = ProxyState::new(controller.clone(), config_rx.clone(), api_key.clone());
        state.lan_bound = lan_bound;
        let stop = shutdown.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = server::serve(listener, state, stop).await {
                warn!(error = %e, "proxy stopped with an error");
            }
        }));

        // GPU sampler.
        if let Some(gpu) = services.gpu {
            tasks.push(tokio::spawn(sampler::run(
                gpu,
                controller.clone(),
                config_rx.clone(),
                events.clone(),
                shutdown.clone(),
            )));
        }

        // Detectors forward their signals to the controller.
        let (sig_tx, sig_rx) = mpsc::unbounded_channel::<GameSignal>();
        let mut detectors = services.detectors;
        let rescan_triggers = services.rescan_triggers;
        for det in &mut detectors {
            if let Err(e) = det.start(sig_tx.clone(), config_rx.clone()) {
                warn!(detector = det.name(), error = %e, "game detector failed to start");
            }
        }
        tasks.push(tokio::spawn(forward_signals(sig_rx, controller.clone())));

        // Hugging Face client and download queue (spec §15). The token is read
        // from the secret store on each request so a newly saved one applies at once.
        let token_source: crate::hf::api::TokenSource = {
            let secrets = secrets.clone();
            Arc::new(move || secrets.get("hf_token"))
        };
        let hf = HfApi::new(services.hf_base_url.as_deref().unwrap_or(HUB_URL), token_source)
            .map_err(|e| CoreError::Other(e.to_string()))?;
        let downloads = DownloadManager::spawn(hf.clone(), config_rx.clone(), events.clone());
        // Downloads may pause while a game runs (downloads.pause_while_gaming).
        {
            let (downloads, mut rx) = (downloads.clone(), events.subscribe());
            let stop = shutdown.clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = stop.cancelled() => break,
                        ev = rx.recv() => match ev {
                            Ok(AppEvent::Gate { game, override_active, .. }) => downloads.set_gaming(game.is_some() && !override_active),
                            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        },
                    }
                }
            }));
        }

        let inner = Arc::new(Inner {
            engine_install: updater::ProgressCell::with_events(events.clone()),
            engine_releases: Mutex::default(),
            paths: paths.clone(),
            config_tx,
            controller: controller.clone(),
            events: events.clone(),
            secrets,
            api_key,
            catalog,
        });

        // Hand edits to config.json are applied live (spec §14.5).
        let watcher = {
            let handle = tokio::runtime::Handle::current();
            let inner = inner.clone();
            match store::watch(&ConfigPaths::new(&paths.config_dir), move || {
                let inner = inner.clone();
                handle.spawn(async move { inner.reload_from_disk() });
            }) {
                Ok(w) => Some(w),
                Err(e) => {
                    warn!(error = %e, "config file watching is unavailable");
                    None
                }
            }
        };

        // After a sleep, rescan games, resample VRAM and health-check models (spec §21).
        #[cfg(windows)]
        let power = if auto_platform {
            let handle = controller.clone();
            crate::platform::windows::power::ResumeWatcher::new(move || {
                info!("the PC woke from sleep");
                handle.system_resumed();
                rescan_triggers.iter().for_each(|t| t.notify_one());
            })
            .map_err(|e| warn!(error = %e, "sleep/resume notifications unavailable"))
            .ok()
        } else {
            None
        };
        #[cfg(not(windows))]
        let _ = rescan_triggers;

        info!(addr = %proxy_addr, "VRAM Gatekeeper started");
        Ok(Self {
            #[cfg(windows)]
            _power: std::sync::Mutex::new(power),
            inner,
            controller,
            events,
            logs,
            hf,
            downloads,
            proxy_addr,
            lan_bound,
            startup,
            gpu_notice,
            shutdown,
            tasks,
            _watcher: std::sync::Mutex::new(watcher),
            detectors: std::sync::Mutex::new(detectors),
        })
    }

    /// The configuration currently in effect.
    pub fn config(&self) -> Arc<Config> {
        self.inner.config_tx.borrow().clone()
    }

    /// Validates and saves `config`, then applies it.
    ///
    /// # Errors
    /// Returns the validation problems (with field paths) without saving, or
    /// a single issue describing a save failure.
    pub fn save_config(&self, config: Config) -> Result<(), Vec<ValidationIssue>> {
        // Warnings and notes never block a save (spec §14.4).
        let errors = errors_of(&validate(&config, ValidateOptions { check_files: true }));
        if !errors.is_empty() {
            return Err(errors);
        }
        store::save(&ConfigPaths::new(&self.inner.paths.config_dir), &config)
            .map_err(|e| vec![ValidationIssue::new(String::new(), e.to_string())])?;
        self.inner.apply(config);
        Ok(())
    }

    /// Validates `config` without saving (live form feedback).
    pub fn validate_config(&self, config: &Config) -> Vec<ValidationIssue> {
        let mut issues = validate(config, ValidateOptions { check_files: true });
        // Findings that need the installed engine's parameter list are not part
        // of `validate`, which has no catalog.
        let catalog = self.inner.catalog.read_ok();
        for (i, model) in config.models.iter().enumerate() {
            for found in crate::speculative::findings(model, &format!("models[{i}]"), Some(&catalog), true) {
                if !issues.contains(&found) {
                    issues.push(found);
                }
            }
        }
        issues
    }

    /// Where config, state and logs live.
    pub fn paths(&self) -> &AppPaths {
        &self.inner.paths
    }

    /// Secret storage (Credential Manager in production).
    pub fn secrets(&self) -> Arc<dyn SecretStore> {
        self.inner.secrets.clone()
    }

    /// Builds the exact command line for `model` using the current settings
    /// (the draft may differ from what is saved).
    pub fn preview_launch(&self, model: &crate::config::schema::ModelConfig) -> LaunchPreview {
        let cfg = self.config();
        let catalog = self.inner.catalog.read_ok();
        let exe = resolve_engine_dir(&cfg, self.inner.paths.bundled_engine_dir.as_deref()).map_or_else(
            |_| "llama-server.exe".to_owned(),
            |d| d.join("llama-server.exe").to_string_lossy().into_owned(),
        );
        let mut warnings = crate::params::build_argv(&catalog, model).warnings;
        warnings.extend(crate::speculative::launch_args(&catalog, model).warnings);
        let findings = crate::speculative::findings(model, "model", Some(&catalog), true);
        let mut args = crate::supervisor::launcher::build_args(&catalog, &cfg, model, 0, "<key>");
        if let Some(i) = args.iter().position(|a| a == "--port")
            && let Some(slot) = args.get_mut(i + 1)
        {
            "<random>".clone_into(slot);
        }
        LaunchPreview { command: crate::params::render_command_line(&exe, &args), warnings, findings }
    }

    /// Lists a repository with quantisation variants and fit verdicts for the
    /// monitored GPU (spec §15.2).
    ///
    /// # Errors
    /// Returns the Hub error.
    pub async fn repo_view(
        &self,
        repo: &str,
        revision: &str,
    ) -> Result<crate::hf::view::RepoView, crate::hf::api::HfError> {
        let listing = self.hf.listing(repo, revision).await?;
        let total = self.controller.status().await.and_then(|s| s.vram).map(|v| v.total_mib).filter(|t| *t > 0);
        Ok(crate::hf::view::build_view(&listing, total))
    }

    /// Suggests draft models for `repo` (spec §10.6 "Find Draft Models").
    ///
    /// # Errors
    /// Returns the Hub error.
    pub async fn find_draft_models(
        &self,
        repo: &str,
    ) -> Result<Vec<crate::hf::draft_search::DraftSuggestion>, crate::hf::api::HfError> {
        use crate::hf::api::SearchSort;
        crate::hf::draft_search::find_draft_models(repo, |query| async move {
            self.hf.search(&query, SearchSort::Downloads, 20).await
        })
        .await
    }

    /// Attaches a finished download to an existing model as its draft file,
    /// replacing any draft it had (spec §10.6 "Add as a Draft Model").
    ///
    /// # Errors
    /// Returns the validation problems if the model is unknown or cannot be saved.
    pub fn add_downloaded_draft(
        &self,
        info: &crate::hf::queue::DownloadInfo,
        model_id: &str,
    ) -> Result<(), Vec<ValidationIssue>> {
        let mut cfg = (*self.config()).clone();
        let Some(model) = cfg.models.iter_mut().find(|m| m.id == model_id) else {
            return Err(vec![ValidationIssue::new("", format!("'{model_id}' is not in the configuration."))]);
        };
        let Some(path) = info.paths.first() else {
            return Err(vec![ValidationIssue::new("", "The download has no file yet.")]);
        };
        crate::speculative::attach_draft(model, path);
        self.save_config(cfg)
    }

    /// Files in the models folder with the models that use them (spec §15.5).
    pub fn library(&self) -> Vec<crate::hf::library::LibraryFile> {
        let cfg = self.config();
        crate::hf::library::scan(&crate::hf::queue::models_dir(&cfg), &cfg)
    }

    /// Deletes a library file (and its split siblings), refusing files in use.
    ///
    /// # Errors
    /// Returns a message explaining why it was refused or failed.
    pub fn delete_library_file(&self, path: &std::path::Path) -> Result<usize, String> {
        let cfg = self.config();
        crate::hf::library::delete(&crate::hf::queue::models_dir(&cfg), &cfg, path)
    }

    /// Creates a model entry from a finished download and saves it (spec §15.4).
    /// Returns the new model's id.
    ///
    /// # Errors
    /// Returns the validation problems if the entry cannot be saved.
    pub fn add_downloaded_model(&self, info: &crate::hf::queue::DownloadInfo) -> Result<String, Vec<ValidationIssue>> {
        let mut cfg = (*self.config()).clone();
        let existing: Vec<String> =
            cfg.models.iter().flat_map(|m| std::iter::once(m.id.clone()).chain(m.aliases.clone())).collect();
        let paths: Vec<PathBuf> = info.paths.iter().map(PathBuf::from).collect();
        let model = crate::hf::library::model_from_download(&info.to_spec(), &paths, &existing);
        let id = model.id.clone();
        cfg.models.push(model);
        self.save_config(cfg)?;
        Ok(id)
    }

    /// Checks Hugging Face models for a newer version (spec §15.5): every
    /// one, or only `only` when given. Runs only when the user asks; one
    /// failing repository does not stop the rest, and results keep config order.
    pub async fn check_model_updates(&self, only: Option<&str>) -> Vec<crate::hf::update::ModelUpdate> {
        use crate::config::schema::ModelSource;
        use crate::hf::update::{ModelUpdate, Verdict, judge, local_size_near};
        use futures_util::StreamExt;
        /// Hub listings fetched at once; low enough not to trip rate limits.
        const CONCURRENT_LISTINGS: usize = 4;

        let cfg = self.config();
        let checks: Vec<_> = cfg
            .models
            .iter()
            .filter(|m| only.is_none_or(|id| m.id == id))
            .filter_map(|model| {
                let ModelSource::Huggingface { repo, revision, commit_sha, files } = model.source.as_ref()? else {
                    return None;
                };
                Some(async move {
                    let listing = match self.hf.listing(repo, revision).await {
                        Ok(listing) => listing,
                        Err(e) => {
                            return ModelUpdate {
                                model_id: model.id.clone(),
                                repo: repo.clone(),
                                verdict: Verdict::Unknown,
                                latest_sha: None,
                                message: e.to_string(),
                            };
                        }
                    };
                    let near: Vec<std::path::PathBuf> = std::iter::once(&model.files.model)
                        .chain(model.files.mmproj.iter())
                        .chain(model.files.draft.iter())
                        .map(Into::into)
                        .collect();
                    let near_refs: Vec<&std::path::Path> = near.iter().map(std::path::PathBuf::as_path).collect();
                    let (verdict, message) =
                        judge(commit_sha.as_deref(), files, &listing, |name| local_size_near(&near_refs, name));
                    ModelUpdate {
                        model_id: model.id.clone(),
                        repo: repo.clone(),
                        verdict,
                        latest_sha: listing.sha,
                        message,
                    }
                })
            })
            .collect();
        futures_util::stream::iter(checks).buffered(CONCURRENT_LISTINGS).collect().await
    }

    /// The current parameter catalog.
    pub fn catalog(&self) -> ParamCatalog {
        self.inner.catalog.read_ok().clone()
    }

    /// Regenerates the parameter catalog from the selected engine.
    ///
    /// # Errors
    /// Returns an error if the engine cannot be run.
    pub async fn refresh_catalog(&self) -> CoreResult<ParamCatalog> {
        let fresh = build_catalog(&self.inner.paths, &self.config()).await?;
        *self.inner.catalog.write_ok() = fresh.clone();
        Ok(fresh)
    }

    /// The controller's view of one model, if it knows it.
    async fn model_status(&self, id: &str) -> Option<ModelStatus> {
        self.controller.status().await.and_then(|s| s.models.into_iter().find(|m| m.id == id))
    }

    /// Waits until `done` accepts the model's status, for at most `timeout`,
    /// and returns the last status seen. It re-checks whenever a model changes
    /// state and at least every [`WAIT_POLL`], because some facts (the measured
    /// footprint) arrive without an event.
    async fn wait_for_model(
        &self,
        id: &str,
        timeout: std::time::Duration,
        done: impl Fn(Option<&ModelStatus>) -> bool,
    ) -> Option<ModelStatus> {
        // Subscribe first so a change between the check and the wait is not missed.
        let mut events = self.events.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let status = self.model_status(id).await;
            if done(status.as_ref()) || tokio::time::Instant::now() >= deadline {
                return status;
            }
            let changed = async {
                loop {
                    match events.recv().await {
                        Ok(AppEvent::ModelState { .. }) | Err(broadcast::error::RecvError::Lagged(_)) => return,
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
                    }
                }
            };
            tokio::select! {
                () = changed => {}
                () = tokio::time::sleep(WAIT_POLL) => {}
            }
        }
    }

    /// Starts a model with its saved settings, reports how it loaded, and
    /// unloads it again unless it was already running (spec §10.5 item 8).
    ///
    /// The gate still applies: while a game runs the report says so instead of
    /// loading anything.
    pub async fn test_load(&self, id: &str) -> TestLoadReport {
        use std::time::Duration;
        let was_loaded = self.model_status(id).await.is_some_and(|m| m.state == ModelStateKind::Ready);
        let started = std::time::Instant::now();
        if let Err(rejection) = self.controller.load(id).await {
            return TestLoadReport::failed(started.elapsed().as_secs_f64(), rejection.message);
        }
        // `load` is acknowledged when the start begins; wait for the server to
        // become ready (or fail) within the model's own load timeout.
        let limit = cfg_load_timeout(&self.config(), id) + Duration::from_secs(10);
        let settled = self
            .wait_for_model(id, limit, |m| {
                m.is_none_or(|m| matches!(m.state, ModelStateKind::Ready) || !m.state.has_process())
            })
            .await;
        let load_seconds = started.elapsed().as_secs_f64();
        match settled {
            Some(m) if m.state == ModelStateKind::Ready => {}
            Some(m) if m.state.has_process() => {
                return TestLoadReport::failed(load_seconds, "The model did not become ready in time.");
            }
            other => {
                let reason = other.and_then(|m| m.failure);
                return TestLoadReport::failed(
                    load_seconds,
                    reason.unwrap_or_else(|| "The model stopped before it was ready.".into()),
                );
            }
        }
        // The footprint is measured shortly after the server reports ready.
        let status =
            self.wait_for_model(id, Duration::from_secs(6), |m| m.is_none_or(|m| m.footprint_mib.is_some())).await;
        if !was_loaded {
            self.controller.unload(id, false).await;
            // Unloading is acknowledged before the process is gone; wait for it
            // (and the VRAM release) so a second click does not race it.
            self.wait_for_model(id, Duration::from_secs(10), |m| m.is_none_or(|m| m.state == ModelStateKind::Unloaded))
                .await;
        }
        // What the engine printed during this launch; the next launch clears it.
        let speculative = {
            let started = self.logs.speculative_started(id);
            let selected = self
                .config()
                .models
                .iter()
                .find(|m| m.id == id)
                .map(|m| crate::speculative::selected_types(&m.llama_args))
                .unwrap_or_default();
            crate::speculative::confirmations(&selected, &started)
        };
        let Some(m) = status else {
            return TestLoadReport::failed(load_seconds, format!("'{id}' is not in the configuration."));
        };
        let message = match (&m.overflow, m.gpu_layers) {
            (Some(o), _) => format!("Loaded in {load_seconds:.1} s, but it overflowed: {o}"),
            (None, Some((on, total))) if on < total => {
                format!(
                    "Loaded in {load_seconds:.1} s with {on} of {total} layers on the GPU; the rest run on the CPU."
                )
            }
            (None, Some((on, total))) => {
                format!("Loaded in {load_seconds:.1} s with all {on} of {total} layers on the GPU.")
            }
            (None, None) => format!("Loaded in {load_seconds:.1} s. The engine did not report where the layers went."),
        };
        TestLoadReport {
            ok: m.overflow.is_none(),
            load_seconds,
            gpu_layers: m.gpu_layers,
            footprint_mib: m.footprint_mib,
            footprint_estimated: m.footprint_estimated,
            overflow: m.overflow,
            message,
            speculative,
        }
    }

    /// Writes a diagnostics zip to `dest` (spec §20): application logs, the
    /// last 1,000 lines of each model log, the config without secrets or
    /// personal paths, learned state, adapters and versions. Nothing leaves
    /// the machine.
    ///
    /// # Errors
    /// Returns an error if the zip cannot be written.
    pub async fn export_diagnostics(&self, dest: &std::path::Path) -> CoreResult<()> {
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        let cfg = self.config();
        let mut entries = vec![diagnostics::Entry::text("config.json", diagnostics::redacted_json(&*cfg, &home)?)];

        let mut system = format!(
            "app version: {}\nwindows: {}\nengine: {}\nproxy: {}\nconfig source: {:?}\n",
            env!("CARGO_PKG_VERSION"),
            diagnostics::windows_version(),
            self.catalog().engine_version,
            self.proxy_addr,
            self.startup.source,
        );
        #[cfg(windows)]
        match crate::gpu::windows_monitor::list_adapters() {
            Ok(list) => {
                for a in list {
                    system.push_str(&format!(
                        "adapter: {} ({} MiB dedicated, integrated: {})\n",
                        a.name, a.dedicated_mib, a.integrated
                    ));
                }
            }
            Err(e) => system.push_str(&format!("adapters unavailable: {e}\n")),
        }
        if let Some(notice) = &self.gpu_notice {
            system.push_str(&format!("gpu notice: {notice}\n"));
        }
        entries.push(diagnostics::Entry::text("system.txt", diagnostics::redact_paths(&system, &home)));

        if let Ok(state) = std::fs::read_to_string(self.inner.paths.state_file()) {
            entries.push(diagnostics::Entry::text("state.json", diagnostics::redact_paths(&state, &home)));
        }
        let logs_dir = self.inner.paths.data_dir.join("logs");
        for entry in std::fs::read_dir(&logs_dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                entries.push(diagnostics::Entry::text(format!("logs/{name}"), diagnostics::redact_paths(&text, &home)));
            }
        }
        for model in &cfg.models {
            let tail = self.logs.tail(&model.id, diagnostics::MODEL_LOG_LINES).join("\n");
            if !tail.is_empty() {
                let name = format!("model-logs/{}.log", model.id);
                entries.push(diagnostics::Entry::text(name, diagnostics::redact_paths(&tail, &home)));
            }
        }
        let dest = dest.to_owned();
        tokio::task::spawn_blocking(move || diagnostics::write_zip(&dest, &entries))
            .await
            .map_err(|e| CoreError::Other(format!("diagnostics task failed: {e}")))?
    }

    /// Engines this app downloaded earlier.
    pub fn installed_engines(&self) -> Vec<InstalledEngine> {
        updater::list_installed(&self.inner.paths.engines_dir())
    }

    /// The CUDA version of the engine in use, preferred when picking a CUDA
    /// build because a newer major may need a newer driver.
    fn engine_cuda_hint(&self) -> Option<(u32, u32)> {
        resolve_engine_dir(&self.config(), self.inner.paths.bundled_engine_dir.as_deref())
            .ok()
            .and_then(|d| updater::cuda_hint_from_path(&d.display().to_string()))
    }

    /// Looks for a newer llama.cpp build for the user's GPU (or for `backend`).
    /// Runs only when the user asks; nothing is checked in the background.
    ///
    /// # Errors
    /// Returns an error if GitHub cannot be reached or answers badly.
    pub async fn check_engine_update(&self, backend: Option<Backend>) -> CoreResult<UpdateCheck> {
        let cfg = self.config();
        let backend = backend.unwrap_or_else(|| {
            selected_adapter(&cfg)
                .map_or(Backend::Vulkan, |a| updater::recommended_backend(a.vendor_id, a.integrated, a.software))
        });
        let url = std::env::var("GATEKEEPER_ENGINE_RELEASES_URL").unwrap_or_else(|_| updater::RELEASES_URL.to_owned());
        let releases = updater::fetch_releases(&url).await?;
        let check =
            updater::choose_update(&releases, backend, Some(&self.catalog().engine_version), self.engine_cuda_hint());
        *self.inner.engine_releases.lock_ok() = releases;
        Ok(check)
    }

    /// Starts downloading the `backend` build of release `tag` in the
    /// background; progress arrives as [`crate::events::AppEvent::EngineInstall`]
    /// events. The files come from the release list of the last update check,
    /// never from the caller.
    ///
    /// # Errors
    /// Returns an error if another download is already running, or `tag` was not
    /// in the last update check or has no such build.
    pub fn start_engine_install(&self, tag: &str, backend: Backend) -> CoreResult<()> {
        let plan = {
            let releases = self.inner.engine_releases.lock_ok();
            let release = releases
                .iter()
                .find(|r| r.tag == tag)
                .ok_or_else(|| CoreError::Other("Check for a newer engine first, then pick a build.".into()))?;
            updater::plan_for(release, backend, self.engine_cuda_hint())
                .ok_or_else(|| CoreError::Other(format!("{tag} has no {} build.", backend.as_str())))?
        };
        updater::check_plan_is_official(&plan)?;
        if !self.inner.engine_install.try_begin(plan.total_bytes()) {
            return Err(CoreError::Other("An engine download is already running.".into()));
        }
        let (dir, cell) = (self.inner.paths.engines_dir(), self.inner.engine_install.clone());
        tokio::spawn(async move {
            if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                cell.update(|p| {
                    p.finished = true;
                    p.error = Some(format!("{}: {e}", dir.display()));
                });
                return;
            }
            let _ = updater::install(&plan, &dir, &cell).await;
        });
        Ok(())
    }

    /// Progress of the engine download.
    pub fn engine_install_progress(&self) -> InstallProgress {
        self.inner.engine_install.get()
    }

    /// Switches to the engine in `dir`, or back to the bundled one with `None`.
    ///
    /// The folder is validated first, so a broken engine is never saved. Then
    /// the parameter catalog is rebuilt and every model's settings are checked
    /// against it; settings the new engine lacks are listed, not deleted.
    ///
    /// # Errors
    /// Returns an error if the engine does not run or the config cannot be saved.
    pub async fn switch_engine(&self, dir: Option<PathBuf>) -> CoreResult<EngineSwitchReport> {
        let mut cfg = (*self.config()).clone();
        cfg.engine.llama_server_dir = dir.as_ref().map(|d| d.display().to_string());
        let target = resolve_engine_dir(&cfg, self.inner.paths.bundled_engine_dir.as_deref())?;
        validate_engine(&target).await?;
        self.save_config(cfg.clone()).map_err(|issues| {
            CoreError::Other(
                issues.first().map_or_else(|| "the configuration was rejected".to_owned(), |i| i.message.clone()),
            )
        })?;
        let catalog = self.refresh_catalog().await?;
        let mut unsupported = Vec::new();
        for model in &cfg.models {
            for w in crate::params::build_argv(&catalog, model).warnings {
                unsupported.push((model.id.clone(), w.key, w.message));
            }
        }
        Ok(EngineSwitchReport {
            engine_version: catalog.engine_version,
            param_count: catalog.params.len(),
            unsupported,
        })
    }

    /// Records whether an API key exists in `server.api_key_set` without the
    /// file checks of a full save (the key lives in Credential Manager; this is
    /// only the marker).
    ///
    /// # Errors
    /// Returns an error if `config.json` cannot be written.
    pub fn set_api_key_flag(&self, set: bool) -> CoreResult<()> {
        let mut cfg = (*self.config()).clone();
        if cfg.server.api_key_set == set {
            return Ok(());
        }
        cfg.server.api_key_set = set;
        store::save(&ConfigPaths::new(&self.inner.paths.config_dir), &cfg)?;
        self.inner.apply(cfg);
        Ok(())
    }

    /// Stores or clears the LAN API key and updates the proxy.
    ///
    /// # Errors
    /// Returns the secret store's message on failure.
    pub fn set_api_key(&self, key: Option<&str>) -> Result<(), String> {
        let key = key.filter(|k| !k.is_empty());
        if key.is_none() && self.lan_bound {
            return Err("The proxy is listening on the network, so it cannot run without an API key. Turn off LAN access and restart first.".to_owned());
        }
        match key {
            Some(k) => self.inner.secrets.set(SECRET_LAN_API_KEY, k)?,
            None => self.inner.secrets.clear(SECRET_LAN_API_KEY),
        }
        *self.inner.api_key.write_ok() = key.map(str::to_owned);
        Ok(())
    }

    /// Stops detectors and the proxy, kills all models, and waits for tasks.
    pub async fn shutdown(mut self) {
        for d in self.detectors.lock_ok().iter_mut() {
            d.stop();
        }
        self.controller.shutdown().await;
        self.shutdown.cancel();
        for t in self.tasks.drain(..) {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), t).await;
        }
    }
}

impl Inner {
    /// Publishes `config` to every component.
    fn apply(&self, config: Config) {
        let config = Arc::new(config);
        if **self.config_tx.borrow() == *config {
            return; // our own save echoing back through the file watcher
        }
        let _ = self.config_tx.send(config.clone());
        self.controller.set_config(config);
    }

    /// Re-reads the file; an invalid edit is reported and not applied.
    fn reload_from_disk(&self) {
        match store::reload(&ConfigPaths::new(&self.paths.config_dir)) {
            Ok(config) => self.apply(config),
            Err((parse_error, issues)) => {
                warn!(%parse_error, problems = issues.len(), "config.json edit rejected; keeping the previous configuration");
                self.events.publish(AppEvent::ConfigError { parse_error: Some(parse_error), issues });
            }
        }
    }
}

/// Moves detector signals into the controller.
async fn forward_signals(mut rx: mpsc::UnboundedReceiver<GameSignal>, controller: ControllerHandle) {
    while let Some(sig) = rx.recv().await {
        controller.game_signal(sig);
    }
}

/// Loads a cached catalog or generates one; an empty catalog (with a warning)
/// if no engine is available, so the app still starts.
async fn load_catalog(paths: &AppPaths, config: &Config) -> ParamCatalog {
    match build_catalog(paths, config).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "no parameter catalog; engine not available yet");
            ParamCatalog::default()
        }
    }
}

/// Builds the catalog for the configured engine, using a cache keyed by
/// engine version.
async fn build_catalog(paths: &AppPaths, config: &Config) -> CoreResult<ParamCatalog> {
    let dir = resolve_engine_dir(config, paths.bundled_engine_dir.as_deref())?;
    let info = validate_engine(&dir).await?;
    let key: String = info.version.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let cache = paths.engines_dir().join(format!("param-catalog-{key}.json"));
    if let Ok(text) = std::fs::read_to_string(&cache)
        && let Ok(cat) = serde_json::from_str::<ParamCatalog>(&text)
    {
        return Ok(cat);
    }
    let now = log_files::today();
    let catalog = generate_catalog(&dir, &info.version, &now).await?;
    if let Ok(json) = serde_json::to_string_pretty(&catalog) {
        let _ = std::fs::create_dir_all(paths.engines_dir());
        let _ = std::fs::write(&cache, json);
    }
    Ok(catalog)
}

/// Fills unset services with the real Windows ones. Returns a notice when VRAM
/// features end up disabled (spec §21: counters unavailable, integrated GPU).
#[cfg(windows)]
fn fill_platform_services(config: &Config, services: &mut Services) -> Option<String> {
    use crate::detect::foreground::ForegroundDetector;
    use crate::detect::process_scan::{ProcessScanDetector, WindowsProcessLister};
    use crate::detect::steam::SteamDetector;
    use crate::gpu::windows_monitor::WindowsGpuMonitor;
    use crate::platform::windows::credentials::WindowsSecretStore;

    let mut gpu_notice: Option<String> = None;
    if services.secrets.is_none() {
        services.secrets = Some(Arc::new(WindowsSecretStore::new()));
    }
    let mut monitor_for_heuristic = None;
    if services.gpu.is_none() {
        let preferred = config.engine.gpu_adapter_luid.as_deref();
        let monitor = match services.adapters.take() {
            Some(adapters) => WindowsGpuMonitor::from_adapters(&adapters, preferred),
            None => WindowsGpuMonitor::new(preferred),
        };
        match monitor {
            Ok(monitor) => {
                info!(adapter = %monitor.adapter().name, total_mib = monitor.adapter().dedicated_mib, "monitoring GPU memory");
                let monitor = Arc::new(monitor);
                monitor_for_heuristic = Some(monitor.clone());
                services.gpu = Some(monitor);
            }
            Err(e) => {
                warn!(error = %e, "VRAM features are disabled");
                gpu_notice = Some(e.to_string());
            }
        }
    }
    if services.detectors.is_empty() {
        let scan = ProcessScanDetector::new(Arc::new(WindowsProcessLister))
            .with_extra_names(Arc::new(crate::detect::windows_games::windows_game_names));
        services.rescan_triggers.push(scan.scan_trigger());
        // A foreground change wakes the scanner at once (NFR-2).
        let foreground = ForegroundDetector::new(Some(scan.scan_trigger()));
        services.detectors.push(Box::new(scan));
        services.detectors.push(Box::new(foreground));
        services.detectors.push(Box::new(SteamDetector::new()));
        if let Some(m) = monitor_for_heuristic {
            services.detectors.push(Box::new(crate::detect::heavy_gpu::HeavyGpuDetector::new(m)));
        }
    }
    gpu_notice
}

/// Non-Windows builds have no platform services to fill in.
#[cfg(not(windows))]
fn fill_platform_services(_config: &Config, _services: &mut Services) -> Option<String> {
    Some("VRAM and game detection are only available on Windows".to_owned())
}
