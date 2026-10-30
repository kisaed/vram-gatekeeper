/**
 * Types mirroring the Rust values the core sends (serde, snake_case).
 *
 * These are written by hand rather than generated (ADR-0012). A drift test in
 * `crates/gatekeeper-core/tests/ui_types_drift.rs` fails when a Rust field is
 * added or renamed without updating this file.
 */

export type DrainPolicy = 'finish' | 'wait_then_cancel' | 'cancel';
export type PressureScope = 'lru_until_below' | 'all';
export type OverflowAction = 'keep_running' | 'error_and_unload';
export type GamePolicy = 'unload' | 'keep';
export type Theme = 'system' | 'light' | 'dark';

export interface GameExecutable {
  name: string;
  path: string | null;
  enabled: boolean;
}

export interface Config {
  $schema?: string | null;
  schema_version: number;
  server: {
    host: string;
    port: number;
    allow_lan: boolean;
    api_key_set: boolean;
    admin_api_enabled: boolean;
    default_model: string | null;
    require_model_field: boolean;
    max_request_body_mib: number;
    cors_allowed_origins: string[];
    allowed_host_names: string[];
  };
  engine: {
    llama_server_dir: string | null;
    device: string | null;
    gpu_adapter_luid: string | null;
  };
  games: {
    enabled: boolean;
    executables: GameExecutable[];
    ignored_executables: string[];
    detect_process_list: boolean;
    detect_foreground_changes: boolean;
    detect_steam: boolean;
    steam_ignored_app_ids: number[];
    detect_exclusive_fullscreen: boolean;
    detect_windows_game_list: boolean;
    heavy_gpu_heuristic: {
      enabled: boolean;
      min_3d_utilization_percent: number;
      min_dedicated_mib: number;
      sustain_seconds: number;
    };
    scan_interval_seconds: number;
    cooldown_seconds: number;
  };
  draining: { policy: DrainPolicy; timeout_seconds: number };
  vram: {
    pressure: { enabled: boolean; threshold_percent: number; sustain_seconds: number; scope: PressureScope };
    overflow: {
      action: OverflowAction;
      check_layer_offload: boolean;
      check_driver_spill: boolean;
      spill_threshold_mib: number;
    };
    loop_guard: {
      enabled: boolean;
      max_unloads: number;
      window_minutes: number;
      reload_margin_mib: number;
      release_sustain_seconds: number;
    };
    make_room: { enabled: boolean; margin_mib: number };
  };
  errors: { status_code: number; retry_after_seconds: number; include_details: boolean };
  downloads: {
    models_dir: string | null;
    hf_token_set: boolean;
    max_concurrent: number;
    verify_sha256: boolean;
    pause_while_gaming: boolean;
    bandwidth_limit_mbps: number | null;
  };
  ui: {
    start_with_windows: boolean;
    open_window_on_start: boolean;
    theme: Theme;
    notifications: {
      game_gate: boolean;
      vram_unload: boolean;
      overflow: boolean;
      loop_guard: boolean;
      load_failures: boolean;
      downloads: boolean;
    };
  };
  logging: { level: string; retention_days: number };
  advanced: {
    gpu_sample_interval_ms_active: number;
    gpu_sample_interval_ms_idle: number;
    sample_when_idle: boolean;
    health_poll_interval_ms: number;
    stop_release_wait_ms: number;
    load_failure_backoff_max_seconds: number;
  };
  models: ModelConfig[];
  [unknown: string]: unknown;
}

export type ModelSource =
  | { type: 'huggingface'; repo: string; revision: string; commit_sha: string | null; files: string[] }
  | { type: 'local' };

export interface ModelConfig {
  id: string;
  display_name: string;
  aliases: string[];
  enabled: boolean;
  notes: string;
  files: { model: string; mmproj: string | null; draft: string | null };
  source: ModelSource | null;
  lifecycle: {
    idle_unload_minutes: number;
    load_timeout_seconds: number;
    game_policy: GamePolicy;
    pinned: boolean;
    preload_on_start: boolean;
    preload_after_game: boolean;
  };
  vram_overrides: { overflow_action: OverflowAction | null; loop_guard_enabled: boolean | null };
  llama_args: Record<string, string | number | boolean | string[] | null>;
  extra_args: string[];
  env: Record<string, string>;
  [unknown: string]: unknown;
}

export type Severity = 'error' | 'warning' | 'info';

export interface ValidationIssue {
  path: string;
  message: string;
  /** Only errors block saving (spec §14.4). */
  severity: Severity;
}

export interface UiError {
  message: string;
  issues: ValidationIssue[];
  /** Machine-readable kind, when the page reacts to it. */
  code: 'needs_access' | null;
}

export type ModelStateName = 'unloaded' | 'starting' | 'ready' | 'draining' | 'stopping' | 'held' | 'failed';

/** Why a model is being unloaded (`UnloadCause`). */
export type UnloadCause =
  | 'game'
  | 'pause'
  | 'vram_pressure'
  | 'overflow'
  | 'idle_ttl'
  | 'make_room'
  | 'user'
  | 'config_change'
  | 'shutdown';

/** Why new requests for a model are refused (`RejectKind`, the ones the gate reports as status). */
export type BlockedReason = 'game_running' | 'paused' | 'vram_guard';

/** The gate in one word (`GateIndicator`); the tray icon shows the same value. */
export type GateIndicator = 'paused' | 'held' | 'draining' | 'closed' | 'busy' | 'open';

export interface ModelStatus {
  id: string;
  display_name: string;
  state: ModelStateName;
  cause: UnloadCause | null;
  blocked_reason: BlockedReason | null;
  inflight: number;
  queued: number;
  footprint_mib: number | null;
  footprint_estimated: boolean;
  gpu_layers: [number, number] | null;
  overflow: string | null;
  needs_restart: boolean;
  manual_hold: boolean;
  pid: number | null;
  idle_seconds: number;
  failure: string | null;
}

export interface ProcessVram {
  pid: number;
  dedicated_mib: number;
  shared_mib: number;
}

export interface VramSample {
  total_mib: number;
  used_mib: number;
  shared_used_mib: number;
  per_process: ProcessVram[];
}

export interface StatusSnapshot {
  gate: GateIndicator;
  paused: boolean;
  game: string | null;
  game_source: string | null;
  session_active: boolean;
  override_active: boolean;
  cooling_down: boolean;
  models: ModelStatus[];
  vram: VramSample | null;
}

export interface StartupInfo {
  version: string;
  config_source: string;
  issues: ValidationIssue[];
  parse_error: string | null;
  gpu_notice: string | null;
  proxy_addr: string | null;
  lan_bound: boolean;
  startup_error: string | null;
  config_path: string | null;
}

export type ValueKind = 'flag' | 'toggle' | 'enum' | 'integer' | 'float' | 'string' | 'path' | 'list';

export interface ParamSpec {
  key: string;
  flags: string[];
  value_kind: ValueKind;
  placeholder: string | null;
  choices: string[] | null;
  default: string | null;
  env: string | null;
  section: string;
  description: string;
  negation_flag: string | null;
  repeatable: boolean;
  basic: boolean;
  reserved: boolean;
}

export interface ParamCatalog {
  engine_version: string;
  generated_at: string;
  params: ParamSpec[];
}

export interface LaunchPreview {
  command: string;
  warnings: { key: string; message: string }[];
  /** Speculative decoding findings for the model (spec §10.6). */
  findings: ValidationIssue[];
}

export interface GameCheck {
  name: string;
  path: string | null;
  ok: boolean;
  error: string | null;
  warnings: string[];
}

export interface RunningProgram {
  name: string;
  pid: number;
  title: string | null;
  gpu_mib: number;
}

export interface AdapterInfo {
  name: string;
  vendor_id: number;
  luid: string;
  dedicated_mib: number;
  software: boolean;
  integrated: boolean;
}

export type NotificationKind = 'game_gate' | 'vram_unload' | 'overflow' | 'loop_guard' | 'load_failure' | 'download';

/** Events the core publishes (`AppEvent`), tagged by `type`. */
export type CoreEvent =
  | { type: 'model_state'; model: string; from: ModelStateName; to: ModelStateName; cause: UnloadCause | null }
  | { type: 'gate'; paused: boolean; game: string | null; override_active: boolean }
  | ({ type: 'vram' } & VramSample)
  | { type: 'model_log'; model: string; line: string }
  | { type: 'notification'; kind: NotificationKind; title: string; body: string }
  | ({ type: 'download' } & DownloadInfo)
  | ({ type: 'engine_install' } & InstallProgress)
  | { type: 'config_reloaded' }
  | { type: 'config_error'; parse_error: string | null; issues: ValidationIssue[] };

// ---------------------------------------------------------------- downloads

export type SearchSort = 'downloads' | 'likes' | 'updated';
export type Fit = 'fits_fully' | 'partial_cpu_offload' | 'too_big';
export type DownloadState = 'queued' | 'downloading' | 'paused' | 'paused_for_game' | 'done' | 'failed' | 'cancelled';

export interface RepoSummary {
  id: string;
  author: string;
  downloads: number;
  likes: number;
  last_modified: string | null;
  gated: boolean;
}

export interface RepoFile {
  path: string;
  size: number;
  sha256: string | null;
}

export interface VariantView {
  id: string;
  quant: string;
  kind: 'single' | 'split';
  files: RepoFile[];
  total_bytes: number;
  complete: boolean;
  fit: Fit;
}

export interface RepoView {
  sha: string | null;
  variants: VariantView[];
  mmproj: RepoFile[];
  /** Draft sidecar files: `mtp-*`, `dflash-*`, `dspark-*` (spec §10.6). */
  sidecars: RepoFile[];
  total_vram_mib: number | null;
}

/** A repository suggested as a draft model (spec §10.6 "Find Draft Models"). */
export interface DraftSuggestion {
  repo: RepoSummary;
  reason: string;
}

/** What the Speculative Decoding tab shows about a GGUF file (spec §10.6). */
export interface GgufSummary {
  architecture: string | null;
  size_bytes: number;
  block_count: number | null;
  vocab_size: number | null;
  tokenizer_model: string | null;
  nextn_predict_layers: number | null;
  has_mtp_layers: boolean;
  /** The speculative type the engine would infer from the file. */
  detected_kind: string | null;
}

export interface EnqueueRequest {
  repo: string;
  revision: string;
  commit_sha: string | null;
  files: RepoFile[];
  label: string;
}

export interface DownloadInfo {
  id: number;
  label: string;
  repo: string;
  revision: string;
  commit_sha: string | null;
  state: DownloadState;
  bytes_done: number;
  bytes_total: number;
  speed_bps: number;
  current_file: string | null;
  error: string | null;
  /** True when `error` is that the repository needs a licence and an access token. */
  needs_access: boolean;
  paths: string[];
  files: string[];
}

export interface LibraryFile {
  path: string;
  size: number;
  quant: string;
  used_by: string[];
  is_projector: boolean;
}

/** Outcome of a test load (spec §10.5 item 8). */
export interface TestLoadReport {
  ok: boolean;
  load_seconds: number;
  gpu_layers: [number, number] | null;
  footprint_mib: number | null;
  footprint_estimated: boolean;
  overflow: string | null;
  message: string;
  /** One entry per speculative type configured for the model (spec §10.6). */
  speculative: SpecConfirmation[];
}

/** Whether the engine's log confirmed that a speculative type started. */
export interface SpecConfirmation {
  spec_type: string;
  confirmed: boolean;
}

// ------------------------------------------------------------- engine updates

export interface EngineAsset {
  name: string;
  size: number;
  url: string;
  sha256: string | null;
}

/** A kind of llama.cpp build (`Backend`). */
export type EngineBackend = 'cuda' | 'vulkan' | 'cpu';

export interface EnginePlan {
  tag: string;
  backend: EngineBackend;
  assets: EngineAsset[];
}

export interface UpdateCheck {
  current_build: number | null;
  latest_build: number | null;
  newer: boolean;
  plan: EnginePlan | null;
  alternatives: EnginePlan[];
}

export interface InstalledEngine {
  name: string;
  path: string;
}

export interface InstallProgress {
  step: string;
  bytes_done: number;
  bytes_total: number;
  finished: boolean;
  error: string | null;
  installed: string | null;
}

export interface EngineSwitchReport {
  engine_version: string;
  param_count: number;
  unsupported: [string, string, string][];
}

/** Result of checking one model for a newer version (spec §15.5). */
export interface ModelUpdate {
  model_id: string;
  repo: string;
  verdict: 'up_to_date' | 'repo_changed' | 'files_changed' | 'unknown';
  latest_sha: string | null;
  message: string;
}
