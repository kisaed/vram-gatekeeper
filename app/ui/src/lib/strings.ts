/**
 * User-facing wording in one place (spec §23.2), so terms stay consistent and
 * translation stays possible. Formal and professional: no contractions or
 * colloquial phrasing, Title Case for labels, buttons and headings, sentence case
 * for sentences.
 */
import type { ModelStateName } from './types';

/** What each model state means, in the words shown next to the state. */
export const STATE_LABEL: Record<ModelStateName, string> = {
  unloaded: 'Not Loaded',
  starting: 'Loading',
  ready: 'Ready',
  draining: 'Completing Requests',
  stopping: 'Unloading',
  held: 'On Hold',
  failed: 'Load Failed',
};

/** Why a model is being unloaded. */
export const CAUSE_LABEL: Record<string, string> = {
  game: 'a game was started',
  pause: 'the gatekeeper was paused',
  vram_pressure: 'GPU memory was insufficient',
  overflow: 'the model exceeded GPU memory and used system RAM',
  idle_ttl: 'the idle time limit elapsed',
  make_room: 'memory was required for another model',
  user: 'it was unloaded manually',
  config_change: 'its settings were changed',
  shutdown: 'the application is shutting down',
};

/** Pages in the navigation rail, in order. */
export const PAGES = [
  { id: 'status', label: 'Status' },
  { id: 'logs', label: 'Logs' },
  { id: 'models', label: 'Models' },
  { id: 'download', label: 'Download' },
  { id: 'games', label: 'Games' },
  { id: 'vram', label: 'VRAM and Requests' },
  { id: 'engine', label: 'Engine' },
  { id: 'general', label: 'General' },
] as const;

export type PageId = (typeof PAGES)[number]['id'];

/** The rail's sections: monitoring, the model library, the policies, and the application itself. */
export const PAGE_GROUPS: { label: string; pages: PageId[] }[] = [
  { label: 'Monitoring', pages: ['status', 'logs'] },
  { label: 'Library', pages: ['models', 'download'] },
  { label: 'Policies', pages: ['games', 'vram'] },
  { label: 'System', pages: ['engine', 'general'] },
];

/** Page title by id. */
export const PAGE_LABEL = Object.fromEntries(PAGES.map((p) => [p.id, p.label])) as Record<PageId, string>;

/** The page that edits a setting, by its config path, so the rail can mark pages with unsaved edits. */
export function pageOfPath(path: string): PageId {
  if (path.startsWith('models')) return 'models';
  if (path.startsWith('games')) return 'games';
  if (path.startsWith('engine.gpu_adapter_luid') || /^(vram|draining|errors)\b/.test(path)) return 'vram';
  if (path.startsWith('engine')) return 'engine';
  if (path.startsWith('downloads')) return 'download';
  return 'general';
}

/** The gate in words, one line per indicator (the tray icon shows the same value). */
export const GATE_LABEL = {
  open: 'Gate open',
  busy: 'Gate open: a model is loading or running',
  draining: 'Completing requests before unloading',
  held: 'A model is on hold after repeated unloads',
  paused: 'Gatekeeper paused',
  connecting: 'Connecting',
} as const;

/** How a changed setting takes effect (spec §14.5, §17.4). */
export type ApplyScope = 'live' | 'model' | 'app';

export const SCOPE_LABEL: Record<ApplyScope, string> = {
  live: 'Applies immediately',
  model: 'Applies the next time the model is loaded',
  app: 'Applies after the application is restarted',
};

/** Which settings need what, by path prefix. First match wins. */
const SCOPE_RULES: [string, ApplyScope][] = [
  ['server.host', 'app'],
  ['server.port', 'app'],
  ['server.allow_lan', 'app'],
  ['models[', 'model'],
];

/** Determines how the setting at `path` takes effect. */
export function scopeOf(path: string): ApplyScope {
  // Lifecycle and VRAM override settings of a model are read live; the rest
  // of a model (files, llama_args, env) needs a reload.
  if (path.startsWith('models[') && /\.(lifecycle|vram_overrides|enabled|aliases|display_name|notes|id)/.test(path)) {
    return 'live';
  }
  const rule = SCOPE_RULES.find(([prefix]) => path.startsWith(prefix));
  return rule ? rule[1] : 'live';
}
