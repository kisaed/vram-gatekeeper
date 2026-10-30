//! Serde + schemars types for `config.json` (spec §14).
//!
//! Responsibility: define the shape of the settings file. Doc comments on
//! fields double as the descriptions in the generated `config.schema.json`,
//! so they are written for the person hand-editing the file.
//! Not responsible for: cross-field validation (`validate.rs`), disk access
//! (`store.rs`) or default values (`defaults.rs`).
//!
//! Every section derives `Default`, and every field has a serde default, so
//! a hand-written file may omit anything and stay short (spec §14.2).
//!
//! Implements FR-MOD-2, FR-MOD-3.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::defaults as d;

/// Unknown keys found while parsing, preserved on save so a newer config
/// opened by an older app loses nothing (spec §14.5).
pub type ExtraFields = BTreeMap<String, Value>;

/// The whole `config.json` document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Config {
    /// Relative path to the JSON Schema, so editors offer completion.
    #[serde(rename = "$schema")]
    pub schema_ref: Option<String>,
    /// Version of this file's layout. Older versions are migrated on load.
    pub schema_version: u32,
    /// Proxy listener settings.
    pub server: ServerConfig,
    /// Which llama.cpp build to run.
    pub engine: EngineConfig,
    /// Game detection settings.
    pub games: GamesConfig,
    /// What happens to in-flight requests when a model must unload.
    pub draining: DrainingConfig,
    /// VRAM pressure, overflow, loop guard and make-room rules.
    pub vram: VramConfig,
    /// How gated requests are answered.
    pub errors: ErrorsConfig,
    /// Hugging Face download settings.
    pub downloads: DownloadsConfig,
    /// Window, tray and notification settings.
    pub ui: UiConfig,
    /// Logging settings.
    pub logging: LoggingConfig,
    /// Rarely-changed tuning knobs.
    pub advanced: AdvancedConfig,
    /// The models clients can request.
    pub models: Vec<ModelConfig>,
    /// Unknown keys, kept so nothing is lost on save.
    #[serde(flatten)]
    pub extra: ExtraFields,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_ref: Some("./config.schema.json".to_owned()),
            schema_version: d::SCHEMA_VERSION,
            server: ServerConfig::default(),
            engine: EngineConfig::default(),
            games: GamesConfig::default(),
            draining: DrainingConfig::default(),
            vram: VramConfig::default(),
            errors: ErrorsConfig::default(),
            downloads: DownloadsConfig::default(),
            ui: UiConfig::default(),
            logging: LoggingConfig::default(),
            advanced: AdvancedConfig::default(),
            models: Vec::new(),
            extra: ExtraFields::new(),
        }
    }
}

// ---------------------------------------------------------------- server

/// Proxy listener settings (spec §8.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ServerConfig {
    /// Address to bind. Ignored (0.0.0.0 is used) when `allow_lan` is true.
    pub host: String,
    /// TCP port clients connect to.
    pub port: u16,
    /// Listen on all interfaces. Requires an API key.
    pub allow_lan: bool,
    /// Whether an API key is stored in Windows Credential Manager. The key
    /// itself is never written to this file.
    pub api_key_set: bool,
    /// Serve the `/gatekeeper/v1/*` admin endpoints.
    pub admin_api_enabled: bool,
    /// Model used when a request omits `model` on endpoints that allow it.
    pub default_model: Option<String>,
    /// Reject requests without a `model` field instead of using the default.
    pub require_model_field: bool,
    /// Largest request body accepted, in MiB.
    pub max_request_body_mib: u32,
    /// Origins allowed by CORS. Empty disables CORS.
    pub cors_allowed_origins: Vec<String>,
    /// Extra `Host` header values accepted in LAN mode (DNS-rebinding guard).
    pub allowed_host_names: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: d::DEFAULT_HOST.to_owned(),
            port: d::DEFAULT_PORT,
            allow_lan: false,
            api_key_set: false,
            admin_api_enabled: true,
            default_model: None,
            require_model_field: false,
            max_request_body_mib: d::DEFAULT_MAX_BODY_MIB,
            cors_allowed_origins: Vec::new(),
            allowed_host_names: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------- engine

/// llama.cpp engine selection (spec §16).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct EngineConfig {
    /// Folder holding `llama-server.exe`. `null` uses the engine bundled
    /// with the installer.
    pub llama_server_dir: Option<String>,
    /// Value for llama-server's `--device`, e.g. `CUDA0` or `Vulkan0`.
    pub device: Option<String>,
    /// LUID of the GPU adapter to monitor (`null` picks the largest).
    pub gpu_adapter_luid: Option<String>,
}

// ----------------------------------------------------------------- games

/// A game executable on the user's list (spec §11.1, FR-GAME-8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(from = "RawGameExecutable")]
pub struct GameExecutable {
    /// Executable file name, matched case-insensitively. A plain string
    /// such as `"game.exe"` is also accepted when hand-editing.
    pub name: String,
    /// Where the file was browsed from. Informational only; matching uses
    /// the file name because verifying a path would need a process handle.
    pub path: Option<String>,
    /// Temporarily disable the entry without removing it.
    pub enabled: bool,
}

/// Accepts both `"game.exe"` and the full object form when deserializing.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged)]
enum RawGameExecutable {
    /// Bare file name.
    Name(String),
    /// Full object.
    Full {
        name: String,
        #[serde(default)]
        path: Option<String>,
        #[serde(default = "d::yes")]
        enabled: bool,
    },
}

impl From<RawGameExecutable> for GameExecutable {
    fn from(raw: RawGameExecutable) -> Self {
        match raw {
            RawGameExecutable::Name(name) => Self { name, path: None, enabled: true },
            RawGameExecutable::Full { name, path, enabled } => Self { name, path, enabled },
        }
    }
}

/// Opt-in detector for unlisted, borderless games (spec §11.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct HeavyGpuHeuristic {
    /// Turn the heuristic on. Off by default: 3D editors and video tools
    /// can trigger it.
    pub enabled: bool,
    /// Minimum 3D engine utilisation of the foreground app, in percent.
    pub min_3d_utilization_percent: u32,
    /// Minimum dedicated GPU memory of the foreground app, in MiB.
    pub min_dedicated_mib: u32,
    /// Seconds both thresholds must hold.
    pub sustain_seconds: u32,
}

impl Default for HeavyGpuHeuristic {
    fn default() -> Self {
        Self {
            enabled: false,
            min_3d_utilization_percent: d::DEFAULT_HEURISTIC_3D_PERCENT,
            min_dedicated_mib: d::DEFAULT_HEURISTIC_MIN_MIB,
            sustain_seconds: d::DEFAULT_HEURISTIC_SUSTAIN_SECONDS,
        }
    }
}

/// Game detection settings (spec §11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct GamesConfig {
    /// Master switch for game gating.
    pub enabled: bool,
    /// Executables treated as games whenever they run.
    pub executables: Vec<GameExecutable>,
    /// Executable names never treated as games by the heuristic.
    pub ignored_executables: Vec<String>,
    /// Detect games by scanning the process list.
    pub detect_process_list: bool,
    /// Re-scan immediately when the foreground window changes.
    pub detect_foreground_changes: bool,
    /// Treat a running Steam app as a game.
    pub detect_steam: bool,
    /// Steam app ids that are not games (tools run through Steam).
    pub steam_ignored_app_ids: Vec<u32>,
    /// Treat exclusive full-screen Direct3D apps as games.
    pub detect_exclusive_fullscreen: bool,
    /// Import Windows' own game list (experimental).
    pub detect_windows_game_list: bool,
    /// Heavy-GPU heuristic settings.
    pub heavy_gpu_heuristic: HeavyGpuHeuristic,
    /// Seconds between process-list scans.
    pub scan_interval_seconds: u32,
    /// Seconds all signals must stay negative before reopening the gate.
    pub cooldown_seconds: u32,
}

impl Default for GamesConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            executables: Vec::new(),
            ignored_executables: Vec::new(),
            detect_process_list: true,
            detect_foreground_changes: true,
            detect_steam: true,
            steam_ignored_app_ids: Vec::new(),
            detect_exclusive_fullscreen: true,
            detect_windows_game_list: false,
            heavy_gpu_heuristic: HeavyGpuHeuristic::default(),
            scan_interval_seconds: d::DEFAULT_SCAN_INTERVAL_SECONDS,
            cooldown_seconds: d::DEFAULT_COOLDOWN_SECONDS,
        }
    }
}

// -------------------------------------------------------------- draining

/// How in-flight requests are handled when a model must unload (FR-GAME-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DrainPolicy {
    /// Wait for in-flight requests with no time limit.
    Finish,
    /// Wait up to `timeout_seconds`, then cancel the rest.
    WaitThenCancel,
    /// Cancel in-flight requests immediately.
    Cancel,
}

/// Drain settings (spec §7.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct DrainingConfig {
    /// Which policy to apply.
    pub policy: DrainPolicy,
    /// Seconds to wait under `wait_then_cancel`.
    pub timeout_seconds: u32,
}

impl Default for DrainingConfig {
    fn default() -> Self {
        Self { policy: DrainPolicy::WaitThenCancel, timeout_seconds: d::DEFAULT_DRAIN_TIMEOUT_SECONDS }
    }
}

// ------------------------------------------------------------------ vram

/// Which models VRAM pressure unloads (spec §13.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PressureScope {
    /// Unload least-recently-used models one at a time until below threshold.
    LruUntilBelow,
    /// Unload every loaded model.
    All,
}

/// What to do when a model overflows into system RAM (FR-VRAM-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverflowAction {
    /// Log and warn, but keep the model running.
    KeepRunning,
    /// Cancel in-flight requests with an error and unload the model.
    ErrorAndUnload,
}

/// VRAM pressure rule (spec §13.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct PressureConfig {
    /// Enable the rule.
    pub enabled: bool,
    /// Percent of adapter VRAM in use that counts as pressure.
    pub threshold_percent: u32,
    /// Seconds pressure must persist before acting.
    pub sustain_seconds: u32,
    /// Which models to unload.
    pub scope: PressureScope,
}

impl Default for PressureConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_percent: d::DEFAULT_PRESSURE_THRESHOLD_PERCENT,
            sustain_seconds: d::DEFAULT_PRESSURE_SUSTAIN_SECONDS,
            scope: PressureScope::LruUntilBelow,
        }
    }
}

/// Overflow rule (spec §13.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct OverflowConfig {
    /// What to do on overflow.
    pub action: OverflowAction,
    /// Treat CPU-offloaded layers at load time as overflow.
    pub check_layer_offload: bool,
    /// Treat driver spill into shared memory as overflow.
    pub check_driver_spill: bool,
    /// Shared-memory growth in MiB that counts as spill.
    pub spill_threshold_mib: u32,
}

impl Default for OverflowConfig {
    fn default() -> Self {
        Self {
            action: OverflowAction::ErrorAndUnload,
            check_layer_offload: true,
            check_driver_spill: true,
            spill_threshold_mib: d::DEFAULT_SPILL_THRESHOLD_MIB,
        }
    }
}

/// Loop guard (spec §13.3, FR-VRAM-6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LoopGuardConfig {
    /// Enable the guard.
    pub enabled: bool,
    /// Pressure/overflow unloads inside the window that trip the guard.
    pub max_unloads: u32,
    /// Window length in minutes.
    pub window_minutes: u32,
    /// Extra free VRAM (MiB) needed beyond the footprint to release a hold.
    pub reload_margin_mib: u32,
    /// Seconds the free-VRAM condition must hold.
    pub release_sustain_seconds: u32,
}

impl Default for LoopGuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_unloads: d::DEFAULT_LOOP_GUARD_MAX_UNLOADS,
            window_minutes: d::DEFAULT_LOOP_GUARD_WINDOW_MINUTES,
            reload_margin_mib: d::DEFAULT_LOOP_GUARD_RELOAD_MARGIN_MIB,
            release_sustain_seconds: d::DEFAULT_LOOP_GUARD_RELEASE_SUSTAIN_SECONDS,
        }
    }
}

/// Make-room rule (spec §13.4, FR-VRAM-7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct MakeRoomConfig {
    /// Evict idle models LRU-first when a new model would not fit.
    pub enabled: bool,
    /// Headroom in MiB required on top of the footprint.
    pub margin_mib: u32,
}

impl Default for MakeRoomConfig {
    fn default() -> Self {
        Self { enabled: true, margin_mib: d::DEFAULT_MAKE_ROOM_MARGIN_MIB }
    }
}

/// All VRAM rules (spec §13).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct VramConfig {
    /// Pressure rule.
    pub pressure: PressureConfig,
    /// Overflow rule.
    pub overflow: OverflowConfig,
    /// Loop guard.
    pub loop_guard: LoopGuardConfig,
    /// Make-room rule.
    pub make_room: MakeRoomConfig,
}

// ---------------------------------------------------------------- errors

/// How gated requests are answered (spec §8.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ErrorsConfig {
    /// HTTP status for gated requests: 409, 423, 429 or 503.
    pub status_code: u16,
    /// `Retry-After` seconds for game and pause rejections.
    pub retry_after_seconds: u32,
    /// Include the machine-readable `gatekeeper` object in error bodies.
    pub include_details: bool,
}

impl Default for ErrorsConfig {
    fn default() -> Self {
        Self {
            status_code: d::DEFAULT_ERROR_STATUS,
            retry_after_seconds: d::DEFAULT_RETRY_AFTER_SECONDS,
            include_details: true,
        }
    }
}

// ------------------------------------------------------------- downloads

/// Hugging Face download settings (spec §15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct DownloadsConfig {
    /// Folder GGUF files are saved under. `null` uses `%USERPROFILE%\Models\gguf`.
    pub models_dir: Option<String>,
    /// Whether a Hugging Face token is stored in Credential Manager.
    pub hf_token_set: bool,
    /// Downloads running at once.
    pub max_concurrent: u32,
    /// Verify SHA-256 against the Hub's LFS hash.
    pub verify_sha256: bool,
    /// Pause downloads while a game session is active.
    pub pause_while_gaming: bool,
    /// Optional bandwidth limit in megabits per second.
    pub bandwidth_limit_mbps: Option<u32>,
}

impl Default for DownloadsConfig {
    fn default() -> Self {
        Self {
            models_dir: None,
            hf_token_set: false,
            max_concurrent: 1,
            verify_sha256: true,
            pause_while_gaming: false,
            bandwidth_limit_mbps: None,
        }
    }
}

// -------------------------------------------------------------------- ui

/// Which events raise a desktop notification (FR-APP-4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct NotificationsConfig {
    /// Game gate opened or closed.
    pub game_gate: bool,
    /// A model was unloaded for VRAM pressure.
    pub vram_unload: bool,
    /// A model overflowed into system RAM.
    pub overflow: bool,
    /// The loop guard held a model.
    pub loop_guard: bool,
    /// A model failed to load.
    pub load_failures: bool,
    /// A download finished or failed.
    pub downloads: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            game_gate: true,
            vram_unload: true,
            overflow: true,
            loop_guard: true,
            load_failures: true,
            downloads: true,
        }
    }
}

/// Window colour theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Follow Windows.
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

/// Window, tray and notification settings (spec §17).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct UiConfig {
    /// Launch with Windows (FR-APP-2).
    pub start_with_windows: bool,
    /// Open the settings window at startup instead of only the tray icon.
    pub open_window_on_start: bool,
    /// Colour theme.
    pub theme: Theme,
    /// Notification toggles.
    pub notifications: NotificationsConfig,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            start_with_windows: true,
            open_window_on_start: false,
            theme: Theme::System,
            notifications: NotificationsConfig::default(),
        }
    }
}

// --------------------------------------------------------------- logging

/// Logging settings (spec §20).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LoggingConfig {
    /// `error`, `warn`, `info`, `debug` or `trace`.
    pub level: String,
    /// Days to keep rolled log files.
    pub retention_days: u32,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self { level: "info".to_owned(), retention_days: d::DEFAULT_LOG_RETENTION_DAYS }
    }
}

// -------------------------------------------------------------- advanced

/// Rarely-changed tuning knobs (spec §12.3, §9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AdvancedConfig {
    /// GPU sample interval (ms) while a model is loaded or a game runs.
    pub gpu_sample_interval_ms_active: u32,
    /// GPU sample interval (ms) when nothing is loaded.
    pub gpu_sample_interval_ms_idle: u32,
    /// Keep sampling at the idle interval when nothing is loaded.
    pub sample_when_idle: bool,
    /// `/health` poll interval (ms) while a model starts.
    pub health_poll_interval_ms: u32,
    /// Longest wait (ms) for VRAM to be released after a child exits.
    pub stop_release_wait_ms: u32,
    /// Cap (seconds) on exponential load-failure backoff.
    pub load_failure_backoff_max_seconds: u32,
}

impl Default for AdvancedConfig {
    fn default() -> Self {
        Self {
            gpu_sample_interval_ms_active: d::DEFAULT_GPU_SAMPLE_ACTIVE_MS,
            gpu_sample_interval_ms_idle: d::DEFAULT_GPU_SAMPLE_IDLE_MS,
            sample_when_idle: true,
            health_poll_interval_ms: d::DEFAULT_HEALTH_POLL_MS,
            stop_release_wait_ms: d::DEFAULT_STOP_RELEASE_WAIT_MS,
            load_failure_backoff_max_seconds: d::DEFAULT_BACKOFF_MAX_SECONDS,
        }
    }
}

// ----------------------------------------------------------------- models

/// Files that make up a model (spec §10.5).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ModelFiles {
    /// GGUF file (the first shard for split models).
    pub model: String,
    /// Optional vision projector.
    pub mmproj: Option<String>,
    /// Optional draft file for speculative decoding: a draft model, an
    /// EAGLE-3, DFlash or DSpark draft, or an MTP head (spec §10.6).
    pub draft: Option<String>,
}

/// Where a model came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelSource {
    /// Downloaded from Hugging Face.
    Huggingface {
        /// Repository, e.g. `Qwen/Qwen3-8B-GGUF`.
        repo: String,
        /// Branch or tag.
        #[serde(default = "default_revision")]
        revision: String,
        /// Commit the files were downloaded at.
        #[serde(default)]
        commit_sha: Option<String>,
        /// File names within the repo.
        #[serde(default)]
        files: Vec<String>,
    },
    /// A file the user supplied.
    Local,
}

/// Default Hugging Face revision.
fn default_revision() -> String {
    "main".to_owned()
}

/// Whether a model is unloaded while a game runs (FR-GAME-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GamePolicy {
    /// Unload while a game runs (default).
    Unload,
    /// Stay available, e.g. a small CPU-only model.
    Keep,
}

/// Per-model lifecycle settings (spec §10.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Lifecycle {
    /// Minutes idle before unloading. 0 means never.
    pub idle_unload_minutes: u32,
    /// Seconds a request waits for the model to load.
    pub load_timeout_seconds: u32,
    /// Behaviour while a game runs.
    pub game_policy: GamePolicy,
    /// Never evict this model to make room for another.
    pub pinned: bool,
    /// Load when the app starts.
    pub preload_on_start: bool,
    /// Load after a game session ends.
    pub preload_after_game: bool,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            idle_unload_minutes: d::DEFAULT_IDLE_UNLOAD_MINUTES,
            load_timeout_seconds: d::DEFAULT_LOAD_TIMEOUT_SECONDS,
            game_policy: GamePolicy::Unload,
            pinned: false,
            preload_on_start: false,
            preload_after_game: false,
        }
    }
}

/// Optional per-model overrides of global VRAM behaviour; `null` inherits.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct VramOverrides {
    /// Override `vram.overflow.action`.
    pub overflow_action: Option<OverflowAction>,
    /// Override `vram.loop_guard.enabled`.
    pub loop_guard_enabled: Option<bool>,
}

/// One model clients can request (spec §10, §14.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ModelConfig {
    /// Name clients send in the `model` field. 1–64 chars of `a-zA-Z0-9._:-`.
    pub id: String,
    /// Friendly name shown in the UI.
    pub display_name: String,
    /// Extra names that route to this model.
    pub aliases: Vec<String>,
    /// Disabled models are hidden from clients.
    pub enabled: bool,
    /// Free-form notes.
    pub notes: String,
    /// Model files.
    pub files: ModelFiles,
    /// Origin of the files.
    pub source: Option<ModelSource>,
    /// Lifecycle settings.
    pub lifecycle: Lifecycle,
    /// VRAM behaviour overrides.
    pub vram_overrides: VramOverrides,
    /// llama-server parameters keyed by long flag name without dashes
    /// (spec §10.4). Unset keys use llama-server's own defaults.
    pub llama_args: BTreeMap<String, Value>,
    /// Extra argv entries appended verbatim, one per element.
    pub extra_args: Vec<String>,
    /// Extra environment variables for this model's process.
    pub env: BTreeMap<String, String>,
    /// Unknown keys, kept so nothing is lost on save.
    #[serde(flatten)]
    pub extra: ExtraFields,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            display_name: String::new(),
            aliases: Vec::new(),
            enabled: true,
            notes: String::new(),
            files: ModelFiles::default(),
            source: None,
            lifecycle: Lifecycle::default(),
            vram_overrides: VramOverrides::default(),
            llama_args: BTreeMap::new(),
            extra_args: Vec::new(),
            env: BTreeMap::new(),
            extra: ExtraFields::new(),
        }
    }
}

impl Config {
    /// Finds an enabled model by id or alias, case-insensitively (spec §8.3).
    pub fn find_model(&self, name: &str) -> Option<&ModelConfig> {
        self.models
            .iter()
            .filter(|m| m.enabled)
            .find(|m| m.id.eq_ignore_ascii_case(name) || m.aliases.iter().any(|a| a.eq_ignore_ascii_case(name)))
    }

    /// Generates the JSON Schema for `config.json` (spec D7).
    pub fn json_schema() -> Value {
        // Serialising a schema built by schemars cannot fail: it contains
        // only strings, numbers and maps.
        serde_json::to_value(schemars::schema_for!(Config)).unwrap_or(Value::Null)
    }
}
