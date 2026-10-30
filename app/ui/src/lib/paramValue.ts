/**
 * Reading and writing one parameter inside a model's `llama_args` (spec §10.4).
 *
 * A parameter may be stored under any of its spellings (`n-gpu-layers`, `ngl`),
 * so lookups go through the catalog entry's flags. Unset parameters are simply
 * absent, which makes llama-server use its own default.
 */
import type { ModelConfig, ParamSpec } from './types';

const norm = (s: string): string => s.replace(/^-+/, '');

/** The key currently used in `llama_args` for this parameter, under any alias. */
export function keyFor(model: ModelConfig, spec: ParamSpec): string | undefined {
  const names = new Set(spec.flags.map(norm));
  return Object.keys(model.llama_args).find((k) => names.has(norm(k)));
}

/** The configured value, or `undefined` when the parameter is not set. */
export function currentValue(model: ModelConfig, spec: ParamSpec): unknown {
  const k = keyFor(model, spec);
  return k === undefined ? undefined : model.llama_args[k];
}

/** Sets a value, or removes the key when the value is empty. */
export function setValue(model: ModelConfig, spec: ParamSpec, value: unknown): void {
  const k = keyFor(model, spec) ?? spec.key;
  if (value === undefined || value === null || value === '' || (Array.isArray(value) && value.length === 0)) {
    delete model.llama_args[k];
  } else {
    model.llama_args[k] = value as never;
  }
}

/** Number, or the words `auto` / `all` for options that take either. */
export function parseLoose(spec: ParamSpec, text: string): unknown {
  const t = text.trim();
  if (t === '') return undefined;
  if (spec.placeholder?.toUpperCase() === 'N' && /^-?\d+(\.\d+)?$/.test(t)) return Number(t);
  return t;
}
