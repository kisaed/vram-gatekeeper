<!--
  ParamEditor: edits every llama-server parameter of the installed engine,
  generated from the parameter catalog (spec §10.2, §10.5).

  Props:  model (the draft ModelConfig; edited in place), catalog
  Emits:  nothing; it changes model.llama_args directly.
  Calls:  no commands.

  Unset parameters are simply absent from llama_args, so llama-server uses its
  own default. Parameters the app manages (host, port, model file...) are never
  shown for editing. Keys the installed engine does not know are listed with a
  warning and kept in the file (spec §10.4). Each parameter shows a readable name
  (lib/paramLabels.ts); its explanation and original flags appear on hover.
-->
<script lang="ts">
  import { humanizeFlags, paramHelp, paramLabel, sectionLabel } from '../lib/paramLabels';
  import { currentValue, keyFor as keyForParam, parseLoose as parseLooseValue, setValue as setParamValue } from '../lib/paramValue';
  import type { ModelConfig, ParamCatalog, ParamSpec } from '../lib/types';
  import Hint from './Hint.svelte';

  let { model, catalog }: { model: ModelConfig; catalog: ParamCatalog } = $props();

  let query = $state('');
  let showAll = $state(false);

  const norm = (s: string) => s.replace(/^-+/, '');

  const keyFor = (spec: ParamSpec) => keyForParam(model, spec);
  const current = (spec: ParamSpec) => currentValue(model, spec);
  const setValue = (spec: ParamSpec, value: unknown) => setParamValue(model, spec, value);
  const parseLoose = parseLooseValue;

  function isSet(spec: ParamSpec): boolean {
    return keyFor(spec) !== undefined;
  }

  /** Search covers the readable name, the original command-line flags and the engine's description. */
  const matches = (spec: ParamSpec, q: string) =>
    !q ||
    paramLabel(spec).toLowerCase().includes(q) ||
    spec.flags.some((f) => f.toLowerCase().includes(q)) ||
    spec.description.toLowerCase().includes(q);

  const visible = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return catalog.params.filter((p) => !p.reserved && matches(p, q) && (q !== '' || showAll || p.basic || isSet(p)));
  });

  const sections = $derived.by(() => {
    const groups = new Map<string, ParamSpec[]>();
    for (const p of visible) {
      const list = groups.get(p.section) ?? [];
      list.push(p);
      groups.set(p.section, list);
    }
    return [...groups.entries()];
  });

  const unsupported = $derived(
    Object.keys(model.llama_args).filter((k) => !catalog.params.some((p) => p.flags.some((f) => norm(f) === norm(k)))),
  );

  const reservedCount = $derived(catalog.params.filter((p) => p.reserved).length);
  const changedCount = $derived(Object.keys(model.llama_args).length);
</script>

<div class="params">
  <div class="tools row">
    <input type="search" class="grow" placeholder="Search Parameters by Name or Description" bind:value={query} aria-label="Search parameters" />
    <label class="row"><input type="checkbox" bind:checked={showAll} /> Show All {catalog.params.length - reservedCount} Parameters</label>
  </div>
  <p class="muted">
    {changedCount} configured. Engine {catalog.engine_version}.
    <Hint
      label="About model parameters"
      text={`A parameter that is not configured uses the default value of llama-server. The default is listed in the description of each parameter.\n${reservedCount} parameters (such as the port, the API key and the model file) are managed by VRAM Gatekeeper and cannot be set here.`}
    />
  </p>

  {#if unsupported.length > 0}
    <div class="notice warn" role="alert">
      The installed engine does not support the following parameters. They are retained but not applied:
      {#each unsupported as k, i (k)}<code>{k}</code>{i < unsupported.length - 1 ? ', ' : ''}{/each}.
      <button class="quiet small" onclick={() => unsupported.forEach((k) => delete model.llama_args[k])}>Remove Them</button>
    </div>
  {/if}

  {#if catalog.params.length === 0}
    <div class="notice">The engine parameters are not yet available. Select an engine on the Engine page.</div>
  {/if}

  {#each sections as [section, specs] (section)}
    <h3 class="group">{sectionLabel(section)}</h3>
    <div class="list">
      {#each specs as spec (spec.key)}
        {@const set = isSet(spec)}
        {@const name = paramLabel(spec)}
        <div class="param" class:set>
          <div class="head">
            <span class="name">{name}<Hint text={paramHelp(spec, catalog.params)} label={`About ${name}`} /></span>
            {#if set}<button class="quiet small" onclick={() => setValue(spec, undefined)}>Reset</button>{/if}
          </div>
          <div class="ctl">
            {#if spec.value_kind === 'flag'}
              <label class="row">
                <input type="checkbox" checked={current(spec) === true} onchange={(e) => setValue(spec, e.currentTarget.checked ? true : undefined)} />
                Enabled
              </label>
            {:else if spec.value_kind === 'toggle'}
              <select
                aria-label={name}
                value={current(spec) === true ? 'on' : current(spec) === false ? 'off' : ''}
                onchange={(e) => setValue(spec, e.currentTarget.value === '' ? undefined : e.currentTarget.value === 'on')}
              >
                <option value="">Use Default</option>
                <option value="on">Enabled</option>
                <option value="off">Disabled</option>
              </select>
            {:else if spec.value_kind === 'enum' && spec.choices}
              <select aria-label={name} value={String(current(spec) ?? '')} onchange={(e) => setValue(spec, e.currentTarget.value || undefined)}>
                <option value="">Use Default</option>
                {#each spec.choices as c (c)}<option value={c}>{c}</option>{/each}
              </select>
            {:else if spec.value_kind === 'integer' || spec.value_kind === 'float'}
              <input
                type="number"
                aria-label={name}
                step={spec.value_kind === 'float' ? 'any' : '1'}
                value={current(spec) ?? ''}
                placeholder={humanizeFlags(spec.default ?? '', catalog.params)}
                onchange={(e) => setValue(spec, e.currentTarget.value === '' ? undefined : Number(e.currentTarget.value))}
              />
            {:else if spec.value_kind === 'list'}
              <textarea
                class="mono"
                rows="2"
                aria-label={name}
                placeholder="One value per line"
                value={Array.isArray(current(spec)) ? (current(spec) as string[]).join('\n') : String(current(spec) ?? '')}
                onchange={(e) => setValue(spec, e.currentTarget.value.split('\n').map((l) => l.trim()).filter(Boolean))}
              ></textarea>
            {:else}
              <input
                type="text"
                class="mono"
                aria-label={name}
                value={String(current(spec) ?? '')}
                placeholder={humanizeFlags(spec.default ?? '', catalog.params)}
                onchange={(e) => setValue(spec, parseLoose(spec, e.currentTarget.value))}
              />
            {/if}
          </div>
        </div>
      {/each}
    </div>
  {/each}

  {#if catalog.params.length > 0 && visible.length === 0}
    <p class="muted">No parameters match the search. Use fewer words, or select Show All.</p>
  {/if}
</div>

<style>
  .params {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .group {
    margin-top: 10px;
    color: var(--ink-2);
  }
  .param {
    padding: 7px 0 7px 12px;
    border-left: 3px solid transparent;
    display: grid;
    align-items: center;
    gap: 3px 16px;
    grid-template-columns: minmax(0, 1fr) minmax(180px, 260px);
  }
  .param.set {
    border-left-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 5%, transparent);
  }
  .head {
    grid-column: 1;
    display: flex;
    gap: 10px;
    align-items: baseline;
    flex-wrap: wrap;
  }
  .name {
    font-weight: 500;
  }
  .ctl {
    grid-column: 2;
    align-self: start;
  }
  .ctl :global(input[type='text']),
  .ctl :global(input[type='number']),
  .ctl :global(select),
  .ctl :global(textarea) {
    width: 100%;
  }
  @media (max-width: 760px) {
    .param {
      grid-template-columns: 1fr;
    }
    .ctl {
      grid-column: 1;
    }
  }
</style>
