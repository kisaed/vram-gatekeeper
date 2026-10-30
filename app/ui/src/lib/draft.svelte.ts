/**
 * The settings draft (spec §17.4).
 *
 * Pages edit a private copy of the configuration. "Save" validates and writes
 * it; "Discard" throws the edits away. The draft is shared by every settings
 * page so leaving a page never loses edits, and the app warns before closing
 * with unsaved changes.
 */
import { api, explain, isUiError } from './api';
import { clone } from './format';
import type { Config, ValidationIssue } from './types';

export const draft = $state({
  /** The copy being edited. */
  value: null as Config | null,
  /** The last saved configuration, for change detection. */
  saved: null as Config | null,
  /** Problems reported by the core, by field path. */
  issues: [] as ValidationIssue[],
  saving: false,
  error: null as string | null,
});

/** Loads (or reloads) the configuration from the core, discarding edits. */
export async function loadDraft(): Promise<void> {
  try {
    const cfg = await api.getConfig();
    draft.saved = cfg;
    draft.value = clone(cfg);
    draft.issues = [];
    draft.error = null;
  } catch (e) {
    draft.error = explain(e);
  }
}

// Serialised once per edit and shared by every reader (dirty checks, live validation).
const draftJson = $derived(JSON.stringify(draft.value));
const savedJson = $derived(JSON.stringify(draft.saved));

/** The serialised draft; changes whenever any setting is edited. */
export function draftSnapshot(): string {
  return draftJson;
}

/** True while the draft differs from the saved configuration. */
export function isDirty(): boolean {
  return draft.value !== null && draftJson !== savedJson;
}

/** Flattens two JSON values into the list of paths whose value changed. */
export function changedPaths(a: unknown, b: unknown, prefix = ''): string[] {
  if (a === b) return [];
  const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
  if (Array.isArray(a) && Array.isArray(b) && a.length === b.length) {
    return a.flatMap((_, i) => changedPaths(a[i], b[i], `${prefix}[${i}]`));
  }
  if (isObj(a) && isObj(b)) {
    const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
    return [...keys].flatMap((k) => changedPaths(a[k], b[k], prefix ? `${prefix}.${k}` : k));
  }
  return [prefix];
}

/** Paths that differ between the draft and the saved config. */
export function dirtyPaths(): string[] {
  return draft.value && draft.saved ? changedPaths(draft.saved, draft.value) : [];
}

/** The error for exactly this field path, if any. Warnings and notes do not count. */
export function issueAt(path: string): string | undefined {
  return draft.issues.find((i) => i.path === path && i.severity === 'error')?.message;
}

/** The most serious finding for exactly this field path, if any (errors first). */
export function findingAt(path: string): ValidationIssue | undefined {
  const here = draft.issues.filter((i) => i.path === path);
  return here.find((i) => i.severity === 'error') ?? here.find((i) => i.severity === 'warning') ?? here[0];
}

/** Number of findings that block saving. */
export function errorCount(): number {
  return draft.issues.filter((i) => i.severity === 'error').length;
}

/** Asks the core to validate the current draft (for live form feedback). */
export async function validateDraft(): Promise<void> {
  if (!draft.value) return;
  try {
    draft.issues = await api.validateConfig(draft.value);
  } catch (e) {
    draft.error = explain(e);
  }
}

/** Saves the draft. Returns true on success. */
export async function saveDraft(): Promise<boolean> {
  if (!draft.value) return false;
  draft.saving = true;
  draft.error = null;
  try {
    await api.saveConfig(draft.value);
    draft.saved = clone(draft.value);
    draft.issues = [];
    return true;
  } catch (e) {
    if (isUiError(e)) {
      draft.issues = e.issues;
      draft.error = e.message;
    } else {
      draft.error = explain(e);
    }
    return false;
  } finally {
    draft.saving = false;
  }
}

/** Throws away edits. */
export function discardDraft(): void {
  if (draft.saved) {
    draft.value = clone(draft.saved);
    draft.issues = [];
    draft.error = null;
  }
}

/** Reads a value by path such as `models[0].lifecycle.pinned`. */
export function getAt(root: unknown, path: string): unknown {
  let cur: unknown = root;
  for (const part of path.replaceAll(/\[(\d+)\]/g, '.$1').split('.').filter(Boolean)) {
    if (typeof cur !== 'object' || cur === null) return undefined;
    cur = (cur as Record<string, unknown>)[part];
  }
  return cur;
}

/** True if the setting at `path` differs from its saved value. */
export function isChangedAt(path: string): boolean {
  return JSON.stringify(getAt(draft.value, path)) !== JSON.stringify(getAt(draft.saved, path));
}
