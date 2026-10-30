/** Defaults for objects the UI creates (mirrors the Rust defaults, spec §14.2). */
import type { ModelConfig } from './types';

/** A new model with the same defaults the core would apply. */
export function newModel(id: string, modelFile = ''): ModelConfig {
  return {
    id,
    display_name: '',
    aliases: [],
    enabled: true,
    notes: '',
    files: { model: modelFile, mmproj: null, draft: null },
    source: modelFile ? { type: 'local' } : null,
    lifecycle: {
      idle_unload_minutes: 30,
      load_timeout_seconds: 180,
      game_policy: 'unload',
      pinned: false,
      preload_on_start: false,
      preload_after_game: false,
    },
    vram_overrides: { overflow_action: null, loop_guard_enabled: null },
    // A modest context keeps VRAM use predictable (spec §15.4).
    llama_args: { 'ctx-size': 8192 },
    extra_args: [],
    env: {},
  };
}

/** Suggests an id from a model file path: "Qwen3-8B-Q4_K_M.gguf" becomes "Qwen3-8B-Q4_K_M". */
export function idFromPath(path: string): string {
  const file = path.split(/[\\/]/).pop() ?? '';
  const base = file.replace(/\.gguf$/i, '').replace(/-\d{5}-of-\d{5}$/i, '');
  return base.replaceAll(/[^a-zA-Z0-9._:-]/g, '-').slice(0, 64) || 'model';
}

/** First id of the form base, base-2, base-3... not in `taken`. */
export function uniqueId(base: string, taken: string[]): string {
  const lower = new Set(taken.map((t) => t.toLowerCase()));
  if (!lower.has(base.toLowerCase())) return base;
  for (let n = 2; ; n++) {
    const candidate = `${base}-${n}`.slice(0, 64);
    if (!lower.has(candidate.toLowerCase())) return candidate;
  }
}
