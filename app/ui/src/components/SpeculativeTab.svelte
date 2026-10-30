<!--
  SpeculativeTab: the Speculative Decoding tab of the Models page (spec §10.6).

  Props:  model (the draft ModelConfig; edited in place), base (its JSON path,
          for findings), catalog (the installed engine's parameters)
  Emits:  nothing; it changes model.files.draft and model.llama_args directly.
  Calls:  gguf_summary, speculative_memory

  The types and their tuning live in llama_args under the engine's own names, so
  the Parameters tab shows the same values. The draft file is model.files.draft.
  Findings come from the core's validation (shown here because the draft is
  validated as it changes); only errors block saving.
-->
<script lang="ts">
  import { api, explain } from '../lib/api';
  import { draft } from '../lib/draft.svelte';
  import { bytes, mib } from '../lib/format';
  import { humanizeFlags, paramHelp, paramLabel } from '../lib/paramLabels';
  import { currentValue, parseLoose, setValue } from '../lib/paramValue';
  import {
    DRAFT_TUNING,
    GROUP_LABEL,
    LEGACY_DRAFT_KEYS,
    SPEC_TYPES,
    isDraftType,
    joinTypes,
    offeredTypes,
    parseTypes,
    typeInfo,
    type SpecGroup,
  } from '../lib/speculative';
  import type { GgufSummary, ModelConfig, ParamCatalog, ParamSpec, Severity } from '../lib/types';
  import Field from './Field.svelte';
  import Hint from './Hint.svelte';
  import PathPicker from './PathPicker.svelte';
  import Toggle from './Toggle.svelte';

  let { model, base, catalog }: { model: ModelConfig; base: string; catalog: ParamCatalog } = $props();

  const filters = [{ name: 'GGUF model', extensions: ['gguf'] }];
  const ABOUT =
    'A draft file type needs a smaller model or a converted draft. Multi-Token Prediction uses layers some models contain. N-Gram types need no extra file.\n\nNo type selected means Off. Every other speculative parameter remains available on the Parameters tab.';
  const norm = (s: string) => s.replace(/^-+/, '');

  const selected = $derived(parseTypes(model.llama_args['spec-type']));
  const offered = $derived(offeredTypes(catalog));

  /** Types to list: those the engine offers, plus any selected one it does not (so it can be cleared). */
  const listed = $derived.by(() => {
    const ids = new Set([...(offered ?? []), ...selected]);
    const known = SPEC_TYPES.filter((t) => ids.has(t.id));
    const unknown = [...ids].filter((id) => !SPEC_TYPES.some((t) => t.id === id)).map(typeInfo);
    return [...known, ...unknown];
  });
  const groups = $derived(
    (['draft', 'mtp', 'ngram'] as SpecGroup[])
      .map((g) => ({ group: g, types: listed.filter((t) => t.group === g) }))
      .filter((g) => g.types.length > 0),
  );

  function toggleType(id: string, on: boolean): void {
    const next = on ? [...selected.filter((s) => s !== id), id] : selected.filter((s) => s !== id);
    const value = joinTypes(next);
    if (value === null) delete model.llama_args['spec-type'];
    else model.llama_args['spec-type'] = value;
  }

  const wantsFile = $derived(selected.some(isDraftType));
  const hasFile = $derived(!!model.files.draft?.trim());
  const legacyKey = $derived(Object.keys(model.llama_args).find((k) => LEGACY_DRAFT_KEYS.includes(norm(k))));

  /** Moves a draft file written on the Parameters tab to its own field. */
  function moveLegacy(): void {
    if (legacyKey === undefined) return;
    const value = model.llama_args[legacyKey];
    if (!hasFile && typeof value === 'string' && value.trim()) model.files.draft = value;
    for (const k of Object.keys(model.llama_args)) if (LEGACY_DRAFT_KEYS.includes(norm(k))) delete model.llama_args[k];
  }

  // ----------------------------------------------------------- tuning fields

  const tuningKeys = $derived.by(() => {
    const keys: string[] = [];
    if (selected.some(isDraftType)) keys.push(...DRAFT_TUNING);
    for (const id of selected) keys.push(...typeInfo(id).tuning);
    return [...new Set(keys)];
  });
  /** Only parameters the installed engine lists are shown. */
  const tuning = $derived(tuningKeys.map((k) => catalog.params.find((p) => p.key === k)).filter((p): p is ParamSpec => !!p));

  // ------------------------------------------------------ file summary, memory

  let draftInfo = $state<GgufSummary | null>(null);
  let draftError = $state('');
  let mainInfo = $state<GgufSummary | null>(null);
  let extraMib = $state<number | null>(null);

  /** Reads a GGUF summary after the path has been still for a moment. */
  function summarise(path: string | null | undefined, apply: (s: GgufSummary | null, error: string) => void): (() => void) | void {
    const p = path?.trim();
    if (!p) {
      apply(null, '');
      return;
    }
    const timer = setTimeout(() => {
      api
        .ggufSummary(p)
        .then((s) => apply(s, ''))
        .catch((e) => apply(null, explain(e)));
    }, 300);
    return () => clearTimeout(timer);
  }

  $effect(() => summarise(model.files.draft, (s, e) => ((draftInfo = s), (draftError = e))));
  $effect(() => summarise(model.files.model, (s) => (mainInfo = s)));

  // The estimate follows every edit of the model (debounced).
  $effect(() => {
    const snapshot = JSON.stringify(model);
    const timer = setTimeout(() => {
      api
        .speculativeMemory(JSON.parse(snapshot))
        .then((n) => (extraMib = n))
        .catch(() => (extraMib = null));
    }, 300);
    return () => clearTimeout(timer);
  });

  // --------------------------------------------------------------- findings

  const findings = $derived(
    draft.issues.filter((i) => i.path === `${base}.llama_args.spec-type` || i.path === `${base}.files.draft`),
  );
  const severityLabel: Record<Severity, string> = { error: 'Problem', warning: 'Caution', info: 'Note' };
  const fileInvalid = $derived(findings.some((i) => i.path === `${base}.files.draft` && i.severity === 'error'));
</script>

{#if offered === null}
  <div class="notice" role="status">
    The installed engine does not support speculative decoding. Select a newer engine on the Engine page.
  </div>
{:else}
  <p class="muted">
    Speculative decoding drafts several tokens cheaply, and the main model verifies them together, which speeds up generation.
    <Hint
      label="About speculative decoding"
      text={ABOUT}
    />
  </p>

  {#if mainInfo?.has_mtp_layers}
    <div class="notice" role="status">
      This model contains multi-token prediction layers{mainInfo.nextn_predict_layers ? ` (${mainInfo.nextn_predict_layers})` : ''} and can
      use Multi-Token Prediction without a separate file.
    </div>
  {/if}

  {#each groups as g (g.group)}
    <h3 class="sub">{GROUP_LABEL[g.group]}</h3>
    <div class="types">
      {#each g.types as t (t.id)}
        <Toggle
          bind:checked={() => selected.includes(t.id), (on) => toggleType(t.id, on)}
          label={t.label}
          hint={t.hint}
        />
      {/each}
    </div>
  {/each}
  {#if selected.length === 0}
    <p class="muted">No type is selected, so speculative decoding is off.</p>
  {/if}

  {#if legacyKey !== undefined}
    <div class="notice warn" role="status">
      A draft model is set on the Parameters tab (<code>{legacyKey}</code>), where it is no longer passed to the engine.
      <button class="small" onclick={moveLegacy}>Move to Draft File</button>
    </div>
  {/if}

  {#if wantsFile || hasFile}
    <h3 class="sub">Draft File</h3>
    <Field
      label="Draft File"
      help="The GGUF file of the draft model, the converted draft, or the multi-token prediction head. It is passed to the engine only while a draft type is selected."
    >
      <PathPicker
        bind:value={() => model.files.draft ?? '', (v) => (model.files.draft = v || null)}
        filters={filters}
        title="Choose a Draft File"
        invalid={fileInvalid}
      />
      {#if hasFile && !wantsFile}
        <p class="muted">The file is kept but not used while no draft type is selected.</p>
      {/if}
      {#if draftError}<p class="field-warning">{draftError}</p>{/if}
      {#if draftInfo}
        <dl class="facts">
          <dt>Architecture</dt><dd>{draftInfo.architecture ?? 'Unknown'}</dd>
          <dt>File Size</dt><dd>{bytes(draftInfo.size_bytes)}</dd>
          <dt>Layers</dt><dd>{draftInfo.block_count ?? 'Unknown'}</dd>
          <dt>Vocabulary Size</dt><dd>{draftInfo.vocab_size?.toLocaleString() ?? 'Unknown'}</dd>
          <dt>Appears to Be</dt><dd>{draftInfo.detected_kind ? typeInfo(draftInfo.detected_kind).label : 'A separate model, or not recognizable'}</dd>
        </dl>
      {/if}
    </Field>
  {/if}

  {#if tuning.length > 0}
    <h3 class="sub">Tuning</h3>
    {#each tuning as spec (spec.key)}
      {@const name = paramLabel(spec)}
      {@const value = currentValue(model, spec)}
      <Field label={name} help={paramHelp(spec, catalog.params)}>
        {#if spec.value_kind === 'integer' || spec.value_kind === 'float'}
          <input
            type="number"
            aria-label={name}
            step={spec.value_kind === 'float' ? 'any' : '1'}
            value={value ?? ''}
            placeholder={humanizeFlags(spec.default ?? '', catalog.params)}
            onchange={(e) => setValue(model, spec, e.currentTarget.value === '' ? undefined : Number(e.currentTarget.value))}
          />
        {:else}
          <input
            type="text"
            class="mono"
            aria-label={name}
            value={String(value ?? '')}
            placeholder={humanizeFlags(spec.default ?? '', catalog.params)}
            onchange={(e) => setValue(model, spec, parseLoose(spec, e.currentTarget.value))}
          />
        {/if}
      </Field>
    {/each}
  {/if}

  <h3 class="sub">Status</h3>
  {#if findings.length === 0}
    <p class="muted">No problems were found with these settings.</p>
  {:else}
    <ul class="findings">
      {#each findings as f (f.path + f.message)}
        <li class={f.severity}><strong>{severityLabel[f.severity]}.</strong> {f.message}</li>
      {/each}
    </ul>
  {/if}
  <p>
    <span class="muted">Additional GPU Memory (Estimated):</span>
    {extraMib === null ? 'Unknown' : extraMib === 0 ? 'None' : mib(extraMib)}
    <Hint
      label="About the memory estimate"
      text="The weights of the draft file plus its context cache at the configured context size, counted once because all request slots share one draft context. The measured memory replaces this estimate after the model has loaded once."
    />
  </p>
{/if}

<style>
  .sub {
    margin-top: 16px;
    padding-bottom: 4px;
    border-bottom: 1px solid var(--line);
  }
  .types {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 8px 0;
  }
  .facts {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 2px 16px;
    margin: 8px 0 0;
  }
  .facts dt {
    color: var(--ink-2);
  }
  .facts dd {
    margin: 0;
  }
  .findings {
    list-style: none;
    margin: 4px 0 8px;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .findings li {
    padding: 6px 10px;
    border-left: 3px solid var(--line-strong);
  }
  .findings li.error {
    border-left-color: var(--closed);
    color: var(--closed);
  }
  .findings li.warning {
    border-left-color: var(--drain);
  }
</style>
