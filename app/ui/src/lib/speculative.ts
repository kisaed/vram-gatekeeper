/**
 * Speculative decoding types for the Speculative Decoding tab (spec §10.6).
 *
 * The engine takes `--spec-type` as a comma-separated list, stored in
 * `llama_args['spec-type']`. The identifiers, the readable names and the
 * explanations live here; the tab offers only the types the installed engine
 * lists, and shows a name derived from the identifier for a type it does not know.
 */
import type { ParamCatalog } from './types';

export type SpecGroup = 'draft' | 'mtp' | 'ngram';

export interface SpecTypeInfo {
  /** The engine's identifier, as written in `--spec-type`. */
  id: string;
  /** Readable name. */
  label: string;
  group: SpecGroup;
  /** True when the type cannot work without a draft file. */
  needsFile: boolean;
  /** Explanation shown on hover. */
  hint: string;
  /** Parameters (catalog keys) tuned for this type, shown while it is selected. */
  tuning: string[];
}

export const GROUP_LABEL: Record<SpecGroup, string> = {
  draft: 'Draft File Types',
  mtp: 'Multi-Token Prediction',
  ngram: 'N-Gram Types',
};

export const SPEC_TYPES: SpecTypeInfo[] = [
  {
    id: 'draft-simple',
    label: 'Separate Draft Model',
    group: 'draft',
    needsFile: true,
    hint: 'A smaller model with the same vocabulary proposes several tokens at a time, and the main model verifies them together. This is the most common choice.',
    tuning: [],
  },
  {
    id: 'draft-eagle3',
    label: 'EAGLE-3',
    group: 'draft',
    needsFile: true,
    hint: 'A one-layer draft that reads the hidden states of the main model. The draft must be converted for this exact model, and the engine cannot recognize it from the file, so it must be selected here.',
    tuning: [],
  },
  {
    id: 'draft-dflash',
    label: 'DFlash',
    group: 'draft',
    needsFile: true,
    hint: 'A block-diffusion draft that proposes a whole block of tokens in one pass. The draft must be converted for this exact model.',
    tuning: [],
  },
  {
    id: 'draft-dspark',
    label: 'DSpark',
    group: 'draft',
    needsFile: true,
    hint: 'DFlash with an additional Markov head. The draft must be converted for this exact model; at present only models with a Qwen3 backbone are supported.',
    tuning: [],
  },
  {
    id: 'draft-mtp',
    label: 'Multi-Token Prediction',
    group: 'mtp',
    needsFile: false,
    hint: 'Uses the multi-token prediction layers that some models contain. They may be inside the model file, or in a separate head file selected as the draft file.',
    tuning: [],
  },
  {
    id: 'ngram-simple',
    label: 'N-Gram: Simple',
    group: 'ngram',
    needsFile: false,
    hint: 'Proposes tokens by finding the latest words of the conversation earlier in the same text. No extra model is needed.',
    tuning: ['spec-ngram-simple-size-n', 'spec-ngram-simple-size-m', 'spec-ngram-simple-min-hits'],
  },
  {
    id: 'ngram-map-k',
    label: 'N-Gram: Map (Keys)',
    group: 'ngram',
    needsFile: false,
    hint: 'Remembers which word sequences followed which, keyed by the preceding words, and proposes the remembered continuation.',
    tuning: ['spec-ngram-map-k-size-n', 'spec-ngram-map-k-size-m', 'spec-ngram-map-k-min-hits'],
  },
  {
    id: 'ngram-map-k4v',
    label: 'N-Gram: Map (Keys and Values)',
    group: 'ngram',
    needsFile: false,
    hint: 'Like the keyed map, but remembers several continuations for each key.',
    tuning: ['spec-ngram-map-k4v-size-n', 'spec-ngram-map-k4v-size-m', 'spec-ngram-map-k4v-min-hits'],
  },
  {
    id: 'ngram-mod',
    label: 'N-Gram: Module',
    group: 'ngram',
    needsFile: false,
    hint: 'A hash-based n-gram module that proposes long runs of tokens when the text repeats itself, which suits code and documents.',
    tuning: ['spec-ngram-mod-n-min', 'spec-ngram-mod-n-max', 'spec-ngram-mod-n-match'],
  },
  {
    id: 'ngram-cache',
    label: 'N-Gram: Cache',
    group: 'ngram',
    needsFile: false,
    hint: 'Proposes tokens from a statistics cache of previously seen text.',
    tuning: [],
  },
];

/** Tuning shared by every type that uses a draft model or prediction layers. */
export const DRAFT_TUNING = ['spec-draft-n-max', 'spec-draft-n-min', 'spec-draft-p-min', 'spec-draft-ngl'];

/** `llama_args` spellings of the draft file that older configurations may hold. */
export const LEGACY_DRAFT_KEYS = ['spec-draft-model', 'md', 'model-draft'];

/** A readable name derived from an identifier the app has no name for. */
function derivedLabel(id: string): string {
  return id
    .split('-')
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(' ');
}

/** Information about a type, with a derived name for an unknown identifier. */
export function typeInfo(id: string): SpecTypeInfo {
  return (
    SPEC_TYPES.find((t) => t.id === id) ?? {
      id,
      label: derivedLabel(id),
      group: id.startsWith('ngram-') ? 'ngram' : id.startsWith('draft-') ? 'draft' : 'ngram',
      needsFile: false,
      hint: 'A speculative decoding type that this version of the application has no description for.',
      tuning: [],
    }
  );
}

/** The identifiers named in a `spec-type` value; absent, empty and `none` mean Off. */
export function parseTypes(value: unknown): string[] {
  const text = Array.isArray(value) ? value.join(',') : typeof value === 'string' ? value : '';
  const out: string[] = [];
  for (const part of text.split(',')) {
    const id = part.trim().toLowerCase();
    if (id && id !== 'none' && !out.includes(id)) out.push(id);
  }
  return out;
}

/** The `spec-type` value for `ids`, or `null` for Off (the key is then removed). */
export function joinTypes(ids: string[]): string | null {
  return ids.length ? ids.join(',') : null;
}

/** True for every `draft-*` type, including Multi-Token Prediction. */
export const isDraftType = (id: string): boolean => id.startsWith('draft-');

/**
 * The types the installed engine lists in the placeholder of `--spec-type`, or
 * `null` when the engine has no such option.
 */
export function offeredTypes(catalog: ParamCatalog): string[] | null {
  const spec = catalog.params.find((p) => p.key === 'spec-type');
  if (!spec) return null;
  return (spec.placeholder ?? '')
    .split(',')
    .map((t) => t.trim().toLowerCase())
    .filter((t) => t && t !== 'none');
}
