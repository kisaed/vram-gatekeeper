//! Every default value of `config.json`, in one place (spec §14.2).
//!
//! Responsibility: name and explain each default so `schema.rs` can stay
//! declarative and a reviewer can audit all defaults on one page.
//! Not responsible for: validation (see `validate.rs`).
//!
//! Implements FR-MOD-2.

/// Current `schema_version` written by this build (spec §14.6).
pub const SCHEMA_VERSION: u32 = 1;

/// Default proxy port. 8088 avoids llama-server's 8080 and Ollama's 11434.
pub const DEFAULT_PORT: u16 = 8088;

/// Loopback address; the proxy is never reachable from the LAN unless the
/// user opts in (spec §19).
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// Largest request body the proxy buffers. 64 MiB leaves room for base64
/// images sent to vision models (spec §8.3).
pub const DEFAULT_MAX_BODY_MIB: u32 = 64;

/// Seconds between process-list scans. A Toolhelp snapshot costs about a
/// millisecond, so 3 s keeps idle CPU negligible (spec §11.1).
pub const DEFAULT_SCAN_INTERVAL_SECONDS: u32 = 3;

/// Seconds all game signals must stay negative before the gate reopens, so
/// a launcher restarting a game's exe does not cause a reload storm.
pub const DEFAULT_COOLDOWN_SECONDS: u32 = 30;

/// Drain timeout for the `wait_then_cancel` policy (spec §7.4).
pub const DEFAULT_DRAIN_TIMEOUT_SECONDS: u32 = 30;

/// VRAM use (percent of the adapter total) that counts as pressure. Below
/// 100 because Windows starts spilling before dedicated memory reads full.
pub const DEFAULT_PRESSURE_THRESHOLD_PERCENT: u32 = 90;

/// Seconds pressure must persist before acting; ignores shader-compile
/// spikes and loading screens (spec §13.1).
pub const DEFAULT_PRESSURE_SUSTAIN_SECONDS: u32 = 3;

/// Shared-memory growth (MiB) over the post-load baseline that counts as
/// driver spill (spec §13.2).
pub const DEFAULT_SPILL_THRESHOLD_MIB: u32 = 512;

/// Unloads within the window before the loop guard trips (spec §13.3).
pub const DEFAULT_LOOP_GUARD_MAX_UNLOADS: u32 = 3;

/// Sliding-window length for the loop guard, in minutes.
pub const DEFAULT_LOOP_GUARD_WINDOW_MINUTES: u32 = 10;

/// Extra free VRAM (MiB) required, beyond the model footprint, before a
/// held model is released.
pub const DEFAULT_LOOP_GUARD_RELOAD_MARGIN_MIB: u32 = 1024;

/// Seconds the free-VRAM condition must hold before releasing a held model.
pub const DEFAULT_LOOP_GUARD_RELEASE_SUSTAIN_SECONDS: u32 = 10;

/// Headroom (MiB) required on top of a model's footprint before starting
/// it without evicting others (spec §13.4).
pub const DEFAULT_MAKE_ROOM_MARGIN_MIB: u32 = 512;

/// HTTP status for gated requests. 503 is the conventional "temporarily
/// unavailable".
pub const DEFAULT_ERROR_STATUS: u16 = 503;

/// `Retry-After` seconds sent with game/pause rejections.
pub const DEFAULT_RETRY_AFTER_SECONDS: u32 = 30;

/// Seconds a request waits for a model to become ready (spec §7.3).
pub const DEFAULT_LOAD_TIMEOUT_SECONDS: u32 = 180;

/// Minutes of inactivity before a model is unloaded.
pub const DEFAULT_IDLE_UNLOAD_MINUTES: u32 = 30;

/// Active GPU sampling interval, in ms (spec §12.3).
pub const DEFAULT_GPU_SAMPLE_ACTIVE_MS: u32 = 1_000;

/// Idle GPU sampling interval, in ms (just keeps the status display fresh).
pub const DEFAULT_GPU_SAMPLE_IDLE_MS: u32 = 10_000;

/// `/health` poll interval while a model starts (spec §9.1).
pub const DEFAULT_HEALTH_POLL_MS: u32 = 250;

/// Longest wait for the driver to release VRAM after a child exits
/// (spec §9.2).
pub const DEFAULT_STOP_RELEASE_WAIT_MS: u32 = 3_000;

/// Cap on exponential load-failure backoff (spec §9.3).
pub const DEFAULT_BACKOFF_MAX_SECONDS: u32 = 300;

/// Log retention in days.
pub const DEFAULT_LOG_RETENTION_DAYS: u32 = 7;

/// Heavy-GPU heuristic: minimum 3D engine utilisation percent (spec §11.5).
pub const DEFAULT_HEURISTIC_3D_PERCENT: u32 = 50;

/// Heavy-GPU heuristic: minimum dedicated memory in MiB.
pub const DEFAULT_HEURISTIC_MIN_MIB: u32 = 2048;

/// Heavy-GPU heuristic: seconds the thresholds must hold.
pub const DEFAULT_HEURISTIC_SUSTAIN_SECONDS: u32 = 10;

/// Returns `true`; used for serde defaults of "on by default" flags.
pub const fn yes() -> bool {
    true
}
