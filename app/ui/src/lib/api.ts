/**
 * Typed wrappers for the Tauri commands (spec §17.5).
 *
 * Every function here maps to one command in `app/src-tauri/src/commands.rs`.
 * Errors arrive as `UiError` objects; `explain` turns any thrown value into text.
 */
import { invoke } from '@tauri-apps/api/core';
import type {
  AdapterInfo,
  Config,
  DownloadInfo,
  EngineBackend,
  EngineSwitchReport,
  InstallProgress,
  InstalledEngine,
  UpdateCheck,
  DraftSuggestion,
  EnqueueRequest,
  GgufSummary,
  LibraryFile,
  ModelUpdate,
  RepoSummary,
  RepoView,
  SearchSort,
  TestLoadReport,
  GameCheck,
  LaunchPreview,
  ParamCatalog,
  RunningProgram,
  StartupInfo,
  StatusSnapshot,
  UiError,
  ValidationIssue,
} from './types';

/** Narrows an unknown thrown value to the host's error shape. */
export function isUiError(e: unknown): e is UiError {
  return typeof e === 'object' && e !== null && 'message' in e && 'issues' in e;
}

/** Plain text for any error thrown by a command. */
export function explain(e: unknown): string {
  if (isUiError(e)) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

export const api = {
  getStatus: () => invoke<StatusSnapshot>('get_status'),
  getStartup: () => invoke<StartupInfo>('get_startup'),
  getConfig: () => invoke<Config>('get_config'),
  saveConfig: (config: Config) => invoke<void>('save_config', { config }),
  validateConfig: (config: Config) => invoke<ValidationIssue[]>('validate_config', { config }),
  getParamCatalog: () => invoke<ParamCatalog>('get_param_catalog'),
  refreshCatalog: () => invoke<ParamCatalog>('refresh_catalog'),
  previewLaunch: (model: unknown) => invoke<LaunchPreview>('preview_launch', { model }),
  loadModel: (id: string) => invoke<void>('load_model', { id }),
  unloadModel: (id: string, hold = false) => invoke<void>('unload_model', { id, hold }),
  restartModel: (id: string) => invoke<void>('restart_model', { id }),
  unloadAll: () => invoke<void>('unload_all'),
  allowAnyway: () => invoke<void>('allow_anyway'),
  releaseHold: (id: string) => invoke<void>('release_hold', { id }),
  setPaused: (paused: boolean) => invoke<void>('set_paused', { paused }),
  listRunningProcesses: () => invoke<RunningProgram[]>('list_running_processes'),
  checkGameExecutable: (input: string) => invoke<GameCheck>('check_game_executable', { input }),
  defaultBrowseDir: () => invoke<string>('default_browse_dir'),
  listGpuAdapters: () => invoke<AdapterInfo[]>('list_gpu_adapters'),
  setApiKey: (key: string | null) => invoke<void>('set_api_key', { key }),
  secretIsSet: (name: string) => invoke<boolean>('secret_is_set', { name }),
  setSecret: (name: string, value: string) => invoke<void>('set_secret', { name, value }),
  clearSecret: (name: string) => invoke<void>('clear_secret', { name }),
  getLogs: (source: string, tail: number) => invoke<string[]>('get_logs', { source, tail }),
  openConfigFolder: () => invoke<void>('open_config_folder'),
  testLoad: (id: string) => invoke<TestLoadReport>('test_load', { id }),
  exportDiagnostics: () => invoke<string>('export_diagnostics'),
  checkModelUpdates: (modelId?: string) => invoke<ModelUpdate[]>('check_model_updates', { modelId }),
  checkEngineUpdate: (backend?: EngineBackend) => invoke<UpdateCheck>('check_engine_update', { backend: backend ?? null }),
  installEngine: (tag: string, backend: EngineBackend) => invoke<void>('install_engine', { tag, backend }),
  engineInstallProgress: () => invoke<InstallProgress>('engine_install_progress'),
  listInstalledEngines: () => invoke<InstalledEngine[]>('list_installed_engines'),
  switchEngine: (dir: string | null) => invoke<EngineSwitchReport>('switch_engine', { dir }),
  hfSearch: (query: string, sort: SearchSort) => invoke<RepoSummary[]>('hf_search', { query, sort }),
  hfRepo: (repo: string, revision?: string) => invoke<RepoView>('hf_repo', { repo, revision: revision ?? null }),
  downloadEnqueue: (request: EnqueueRequest) => invoke<number>('download_enqueue', { request }),
  downloadPause: (id: number) => invoke<void>('download_pause', { id }),
  downloadResume: (id: number) => invoke<void>('download_resume', { id }),
  downloadCancel: (id: number) => invoke<void>('download_cancel', { id }),
  downloadDismiss: (id: number) => invoke<void>('download_dismiss', { id }),
  downloadList: () => invoke<DownloadInfo[]>('download_list'),
  libraryList: () => invoke<LibraryFile[]>('library_list'),
  libraryDelete: (path: string) => invoke<number>('library_delete', { path }),
  addDownloadedModel: (downloadId: number) => invoke<string>('add_downloaded_model', { downloadId }),
  addDownloadedDraft: (downloadId: number, modelId: string) =>
    invoke<void>('add_downloaded_draft', { downloadId, modelId }),
  findDraftModels: (repo: string) => invoke<DraftSuggestion[]>('find_draft_models', { repo }),
  ggufSummary: (path: string) => invoke<GgufSummary>('gguf_summary', { path }),
  speculativeMemory: (model: unknown) => invoke<number>('speculative_memory', { model }),
};
