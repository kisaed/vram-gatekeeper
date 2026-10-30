/**
 * Live state shared by all pages: status, startup info, VRAM history and the
 * event feed. Fed by the core's events (while this window exists) and by
 * explicit refreshes.
 */
import { listen } from '@tauri-apps/api/event';
import { api, explain } from './api';
import type { CoreEvent, DownloadInfo, InstallProgress, StartupInfo, StatusSnapshot, VramSample } from './types';

/** One point of the five-minute VRAM graph. */
export interface VramPoint {
  /** Milliseconds since epoch. */
  t: number;
  /** Dedicated VRAM in use on the adapter, MiB. */
  used: number;
  /** Portion used by our own llama-server processes, MiB. */
  models: number;
  /** Adapter total, MiB. */
  total: number;
}

/** One line of the recent-events list. */
export interface FeedItem {
  t: number;
  kind: 'info' | 'warn' | 'error';
  text: string;
}

/** Five minutes of one-second samples. */
const HISTORY_POINTS = 300;
const FEED_LIMIT = 60;

export const live = $state({
  status: null as StatusSnapshot | null,
  startup: null as StartupInfo | null,
  history: [] as VramPoint[],
  feed: [] as FeedItem[],
  /** Set when the core has not answered yet. */
  connecting: true,
  /** Text of the last problem reaching the core. */
  problem: null as string | null,
  /** All downloads, newest last. */
  downloads: [] as DownloadInfo[],
  /** The engine download in flight or the last one that ended; `null` before any. */
  engineInstall: null as InstallProgress | null,
  /** Incremented each time config.json was reloaded from disk. */
  configReloads: 0,
  /** Latest rejected hand edit of config.json. */
  configError: null as { parse_error: string | null; issues: { path: string; message: string }[] } | null,
});

/** Appends to the feed, newest first. */
function note(kind: FeedItem['kind'], text: string): void {
  live.feed = [{ t: Date.now(), kind, text }, ...live.feed].slice(0, FEED_LIMIT);
}

/** Adds a VRAM sample to the graph history. */
function recordVram(sample: VramSample): void {
  const pids = new Set((live.status?.models ?? []).map((m) => m.pid).filter((p): p is number => p !== null));
  const models = sample.per_process.filter((p) => pids.has(p.pid)).reduce((sum, p) => sum + p.dedicated_mib, 0);
  const point: VramPoint = { t: Date.now(), used: sample.used_mib, models, total: sample.total_mib };
  live.history.push(point);
  if (live.history.length > HISTORY_POINTS) live.history.shift();
}

/** Re-reads the status snapshot. */
export async function refreshStatus(): Promise<void> {
  try {
    live.status = await api.getStatus();
    live.problem = null;
    live.connecting = false;
  } catch (e) {
    live.problem = explain(e);
  }
}

/** Re-reads startup facts (config source, proxy address, notices). */
export async function refreshStartup(): Promise<void> {
  try {
    live.startup = await api.getStartup();
  } catch (e) {
    live.problem = explain(e);
  }
}

/** Turns one core event into state. */
function onEvent(ev: CoreEvent): void {
  switch (ev.type) {
    case 'model_state':
      note('info', `Model ${ev.model}: ${ev.from} to ${ev.to}${ev.cause ? ` (${ev.cause.replaceAll('_', ' ')})` : ''}`);
      void refreshStatus();
      break;
    case 'gate':
      note('info', ev.paused ? 'Gatekeeper paused' : ev.game ? `Game running: ${ev.game}` : 'No game running');
      void refreshStatus();
      break;
    case 'vram':
      if (live.status) live.status.vram = ev;
      recordVram(ev);
      break;
    case 'notification':
      note(ev.kind === 'load_failure' || ev.kind === 'overflow' ? 'warn' : 'info', `${ev.title}: ${ev.body}`);
      break;
    case 'download': {
      const { type: _type, ...info } = ev;
      const i = live.downloads.findIndex((d) => d.id === info.id);
      if (i >= 0) live.downloads[i] = info;
      else live.downloads.push(info);
      break;
    }
    case 'engine_install': {
      const { type: _type, ...progress } = ev;
      live.engineInstall = progress;
      break;
    }
    case 'config_reloaded':
      live.configError = null;
      live.configReloads += 1;
      note('info', 'Configuration file reloaded');
      void refreshStatus();
      break;
    case 'config_error':
      live.configError = { parse_error: ev.parse_error, issues: ev.issues };
      note('error', 'The configuration file was changed to an invalid state. The change was not applied.');
      break;
    case 'model_log':
      break; // handled by the Logs page
  }
}

/** Listeners that model logs subscribers can attach to. */
export const logListeners = new Set<(model: string, line: string) => void>();

/**
 * Follows one model's log: `history` stored lines, then live lines, keeping the
 * last `cap`. A load prints hundreds of lines at once, so changes reach `set`
 * at most once per animation frame. Returns the function that stops following.
 */
export function followModelLog(id: string, history: number, cap: number, set: (lines: string[]) => void): () => void {
  let lines: string[] = [];
  let frame = 0;
  const publish = () => {
    frame = 0;
    set(lines.slice());
  };
  void api
    .getLogs(`model:${id}`, history)
    .catch(() => [] as string[])
    .then((stored) => {
      lines = stored;
      publish();
    });
  const listener = (model: string, line: string) => {
    if (model !== id) return;
    lines.push(line);
    if (lines.length > cap) lines.splice(0, lines.length - cap);
    if (!frame) frame = requestAnimationFrame(publish);
  };
  logListeners.add(listener);
  return () => {
    logListeners.delete(listener);
    if (frame) cancelAnimationFrame(frame);
  };
}

let started = false;

/**
 * Reads everything the page shows from the core. If the core is not up yet this
 * does nothing; the host sends `core-resync` when startup ends.
 */
async function syncAll(): Promise<void> {
  await refreshStartup();
  if (live.startup?.startup_error) {
    live.connecting = false;
    return;
  }
  if (!live.startup?.proxy_addr) return;
  await Promise.all([
    refreshStatus(),
    api
      .downloadList()
      .then((l) => (live.downloads = l))
      .catch(() => {}),
    api
      .engineInstallProgress()
      .then((p) => (live.engineInstall = p.step ? p : null))
      .catch(() => {}),
  ]);
}

/** Connects to the core. A window opened before the core is up waits for `core-resync`. */
export async function startLive(): Promise<void> {
  if (started) return;
  started = true;

  await listen<CoreEvent>('core-event', (e) => {
    if (e.payload.type === 'model_log') {
      for (const fn of logListeners) fn(e.payload.model, e.payload.line);
    }
    onEvent(e.payload);
  });
  await listen('core-resync', () => void syncAll());

  await syncAll();
  if (live.connecting) {
    setTimeout(() => {
      if (live.connecting) live.problem ??= 'The gatekeeper did not start within 30 seconds.';
    }, 30_000);
  }
}
