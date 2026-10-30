<!--
  Models page: the list of models and the editor for one (spec §10.5, FR-MOD-1).

  Props:  none
  Emits:  nothing
  Calls:  get_param_catalog, preview_launch, load_model, unload_model,
          restart_model, get_logs (via the draft and live stores)
-->
<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog';
  import { api, explain } from '../lib/api';
  import { paramCatalog } from '../lib/catalog';
  import { draft, isChangedAt, issueAt } from '../lib/draft.svelte';
  import { mib } from '../lib/format';
  import { followModelLog, live } from '../lib/live.svelte';
  import { nav } from '../lib/nav.svelte';
  import { idFromPath, newModel, uniqueId } from '../lib/templates';
  import { typeInfo } from '../lib/speculative';
  import type { LaunchPreview, ModelUpdate, ParamCatalog, TestLoadReport } from '../lib/types';
  import Field from '../components/Field.svelte';
  import Hint from '../components/Hint.svelte';
  import ListEditor from '../components/ListEditor.svelte';
  import NumberField from '../components/NumberField.svelte';
  import ParamEditor from '../components/ParamEditor.svelte';
  import PathPicker from '../components/PathPicker.svelte';
  import SpeculativeTab from '../components/SpeculativeTab.svelte';
  import StatusChip from '../components/StatusChip.svelte';
  import Toggle from '../components/Toggle.svelte';

  type Tab = 'general' | 'files' | 'lifecycle' | 'params' | 'speculative' | 'advanced' | 'launch' | 'logs';
  const TABS: { id: Tab; label: string }[] = [
    { id: 'general', label: 'General' },
    { id: 'files', label: 'Files' },
    { id: 'lifecycle', label: 'Lifecycle' },
    { id: 'params', label: 'Parameters' },
    { id: 'speculative', label: 'Speculative Decoding' },
    { id: 'advanced', label: 'Additional Arguments' },
    { id: 'launch', label: 'Launch and Test' },
    { id: 'logs', label: 'Log' },
  ];

  /** Left and Right move between tabs, Home and End jump to the first and last (spec §17.6). */
  function tabKeys(e: KeyboardEvent): void {
    const i = TABS.findIndex((t) => t.id === tab);
    const next = { ArrowRight: (i + 1) % TABS.length, ArrowLeft: (i - 1 + TABS.length) % TABS.length, Home: 0, End: TABS.length - 1 }[e.key];
    if (next === undefined) return;
    e.preventDefault();
    tab = TABS[next].id;
    document.getElementById(`tab-${tab}`)?.focus();
  }

  let selected = $state(0);
  let tab = $state<Tab>('general');
  let catalog = $state<ParamCatalog | null>(null);
  let preview = $state<LaunchPreview | null>(null);
  let previewError = $state('');
  let logLines = $state<string[]>([]);
  let envKey = $state('');
  let envValue = $state('');
  let confirmDelete = $state(false);
  let testing = $state(false);
  let updateCheck = $state<ModelUpdate | null>(null);
  let updateBusy = $state(false);
  let updateError = $state('');
  let testReport = $state<TestLoadReport | null>(null);
  let testError = $state('');

  const models = $derived(draft.value?.models ?? []);
  const model = $derived(models[selected] ?? null);
  const base = $derived(`models[${selected}]`);
  /** True when the draft of this model differs from what is saved. */
  const dirtyModel = $derived(isChangedAt(base));
  const runtime = $derived(live.status?.models.find((m) => m.id === model?.id) ?? null);

  // Jump to a model another page asked for (for example a fresh download).
  $effect(() => {
    const wanted = nav.modelToSelect;
    if (!wanted) return;
    const i = models.findIndex((m) => m.id === wanted);
    if (i >= 0) {
      selected = i;
      tab = 'general';
      nav.modelToSelect = null;
    }
  });

  // Keep the selection valid when models are removed.
  $effect(() => {
    if (selected >= models.length) selected = Math.max(0, models.length - 1);
  });

  // The parameter catalog is needed by the parameters tab and the preview.
  $effect(() => {
    paramCatalog().then((c) => (catalog = c)).catch(() => (catalog = null));
  });

  // Launch preview follows the draft (debounced).
  $effect(() => {
    if (tab !== 'launch' || !model) return;
    const snapshot = JSON.stringify(model);
    const timer = setTimeout(() => {
      api
        .previewLaunch(JSON.parse(snapshot))
        .then((p) => {
          preview = p;
          previewError = '';
        })
        .catch((e) => (previewError = explain(e)));
    }, 250);
    return () => clearTimeout(timer);
  });

  // Log tab: history first, then live lines.
  $effect(() => {
    if (tab !== 'logs' || !model) return;
    return followModelLog(model.id, 400, 1500, (l) => (logLines = l));
  });

  async function checkUpdate(): Promise<void> {
    if (!model) return;
    updateBusy = true;
    updateError = '';
    updateCheck = null;
    try {
      const [result] = await api.checkModelUpdates(model.id);
      updateCheck = result ?? null;
    } catch (e) {
      updateError = explain(e);
    } finally {
      updateBusy = false;
    }
  }

  async function runTest(): Promise<void> {
    if (!model) return;
    testing = true;
    testReport = null;
    testError = '';
    try {
      testReport = await api.testLoad(model.id);
    } catch (e) {
      testError = explain(e);
    } finally {
      testing = false;
    }
  }

  function add(path = ''): void {
    if (!draft.value) return;
    const id = uniqueId(path ? idFromPath(path) : 'new-model', models.map((m) => m.id));
    draft.value.models.push(newModel(id, path));
    selected = draft.value.models.length - 1;
    tab = path ? 'general' : 'files';
  }

  function remove(): void {
    if (!draft.value) return;
    draft.value.models.splice(selected, 1);
    confirmDelete = false;
  }

  function addEnv(): void {
    if (!model || !envKey.trim()) return;
    model.env[envKey.trim()] = envValue;
    envKey = '';
    envValue = '';
  }

  const filters = [{ name: 'GGUF model', extensions: ['gguf'] }];
</script>

{#if !draft.value}
  <p class="muted">Loading settings.</p>
{:else}
  <div class="models">
    <aside class="side">
      <div class="spread">
        <h1>Models<Hint text="To download models from Hugging Face, use the Download page." label="About adding models" /></h1>
      </div>
      <ul class="picker" aria-label="Models">
        {#each models as m, i (i)}
          {@const rt = live.status?.models.find((x) => x.id === m.id)}
          <li>
            <button class="pick" class:on={i === selected} onclick={() => (selected = i)} aria-current={i === selected}>
              <span class="name">{m.display_name || m.id || 'Unnamed'}</span>
              <span class="muted small">
                {#if !m.enabled}Disabled{:else if rt}<StatusChip state={rt.state} />{:else}Not Saved{/if}
              </span>
            </button>
          </li>
        {/each}
      </ul>
      <div class="stack-sm">
        <button class="primary" onclick={() => add()}>Add a Model</button>
        <button onclick={async () => {
          const picked = await open({ multiple: false, filters, title: 'Choose a GGUF Model' });
          if (typeof picked === 'string') add(picked);
        }}>Add from a File</button>
      </div>
    </aside>

    <section class="editor">
      {#if !model}
        <div class="notice">No models are configured. Add a model by selecting an existing GGUF file, or download one.</div>
      {:else}
        <header class="spread">
          <div>
            <h2>{model.display_name || model.id}</h2>
            <p class="muted mono">{model.id}</p>
          </div>
          {#if runtime}
            <div class="row">
              <StatusChip state={runtime.state} cause={runtime.cause} />
              {#if runtime.state === 'unloaded' || runtime.state === 'failed'}
                <button class="small" onclick={() => api.loadModel(model.id).catch(() => {})}>Load Now</button>
              {:else if runtime.state === 'ready'}
                <button class="small" onclick={() => api.unloadModel(model.id, false)}>Unload</button>
              {/if}
            </div>
          {/if}
        </header>

        <div class="tabs" role="tablist" aria-label="Model settings" tabindex="-1" onkeydown={tabKeys}>
          {#each TABS as t (t.id)}
            <button
              role="tab"
              id={`tab-${t.id}`}
              aria-selected={tab === t.id}
              aria-controls="model-panel"
              tabindex={tab === t.id ? 0 : -1}
              class:on={tab === t.id}
              onclick={() => (tab = t.id)}>{t.label}</button
            >
          {/each}
        </div>

        <div class="panel" role="tabpanel" id="model-panel" aria-labelledby={`tab-${tab}`}>
          {#if tab === 'general'}
            <Field label="Model Name" path={`${base}.id`} help="Clients send this value in the model field of every request. Only letters, digits and the characters . _ : - are permitted.">
              <input type="text" class="mono" bind:value={model.id} aria-invalid={!!issueAt(`${base}.id`)} />
            </Field>
            <Field label="Display Name" path={`${base}.display_name`}>
              <input type="text" bind:value={model.display_name} placeholder="Displayed in this application" />
            </Field>
            <Field label="Alternative Names" help="Additional names that also route requests to this model.">
              <ListEditor bind:items={model.aliases} placeholder="Add an alias, for example default-chat" mono />
            </Field>
            <Field label="Availability">
              <Toggle bind:checked={model.enabled} label="Enabled" hint="A disabled model is hidden from clients and is never loaded." />
            </Field>
            <Field label="Notes">
              <textarea rows="3" bind:value={model.notes}></textarea>
            </Field>
            <div class="danger-zone">
              {#if !confirmDelete}
                <button class="danger" onclick={() => (confirmDelete = true)}>Remove This Model</button>
              {:else}
                <span>Remove "{model.id}" from the settings? The model file is not deleted.</span>
                <button class="danger" onclick={remove}>Remove</button>
                <button onclick={() => (confirmDelete = false)}>Cancel</button>
              {/if}
            </div>
          {:else if tab === 'files'}
            <Field label="Model File" path={`${base}.files.model`} help="The GGUF file. For a split model, select the first part (the file name ends in 00001-of-...).">
              <PathPicker bind:value={model.files.model} filters={filters} title="Choose a GGUF Model" invalid={!!issueAt(`${base}.files.model`)} />
            </Field>
            <Field label="Vision Projector" path={`${base}.files.mmproj`} help="Optional. Required for models that process images.">
              <PathPicker
                bind:value={() => model.files.mmproj ?? '', (v) => (model.files.mmproj = v || null)}
                filters={filters}
                title="Choose a Vision Projector File"
                invalid={!!issueAt(`${base}.files.mmproj`)}
              />
            </Field>
            {#if model.source?.type === 'huggingface'}
              <Field label="Downloaded From" help="Contacts huggingface.co only when an update check is requested.">
                <p>{model.source.repo} <span class="muted">({model.source.revision})</span></p>
                <div class="row">
                  <button class="small" onclick={checkUpdate} disabled={updateBusy}>{updateBusy ? 'Checking' : 'Check for a Newer Version'}</button>
                </div>
                {#if updateError}<div class="notice error" role="alert">{updateError}</div>{/if}
                {#if updateCheck}
                  <div class="notice" class:warn={updateCheck.verdict === 'files_changed'} role="status">{updateCheck.message}</div>
                {/if}
              </Field>
            {/if}
          {:else if tab === 'lifecycle'}
            <Field label="During Games" path={`${base}.lifecycle.game_policy`} help="Small CPU-only models can remain available while a game is running.">
              <select bind:value={model.lifecycle.game_policy}>
                <option value="unload">Unload the model</option>
                <option value="keep">Keep the model available</option>
              </select>
            </Field>
            <Field label="Idle Unload Time" path={`${base}.lifecycle.idle_unload_minutes`} help="A value of 0 disables automatic unloading.">
              <NumberField bind:value={model.lifecycle.idle_unload_minutes} min={0} unit="minutes" />
            </Field>
            <Field label="Load Timeout" path={`${base}.lifecycle.load_timeout_seconds`} help="Time a request waits for the model to load before a timeout error is returned.">
              <NumberField bind:value={model.lifecycle.load_timeout_seconds} min={1} unit="seconds" />
            </Field>
            <Field label="Memory Retention" path={`${base}.lifecycle.pinned`}>
              <Toggle bind:checked={model.lifecycle.pinned} label="Pinned" hint="A pinned model is never unloaded to make room for another model, and is unloaded last under VRAM pressure." />
            </Field>
            <Field label="Preloading">
              <div class="stack-sm">
                <Toggle bind:checked={model.lifecycle.preload_on_start} label="Load When the Application Starts" />
                <Toggle bind:checked={model.lifecycle.preload_after_game} label="Reload After a Game Ends" />
              </div>
            </Field>
            <h3 class="sub">VRAM Behavior for This Model</h3>
            <Field label="Overflow into System RAM" path={`${base}.vram_overrides.overflow_action`}>
              <select
                value={model.vram_overrides.overflow_action ?? ''}
                onchange={(e) => (model.vram_overrides.overflow_action = (e.currentTarget.value || null) as typeof model.vram_overrides.overflow_action)}
              >
                <option value="">Use the global setting</option>
                <option value="keep_running">Continue running</option>
                <option value="error_and_unload">Stop with an error and unload</option>
              </select>
            </Field>
            <Field label="Repeated Unloads" path={`${base}.vram_overrides.loop_guard_enabled`}>
              <select
                value={model.vram_overrides.loop_guard_enabled === null ? '' : String(model.vram_overrides.loop_guard_enabled)}
                onchange={(e) => (model.vram_overrides.loop_guard_enabled = e.currentTarget.value === '' ? null : e.currentTarget.value === 'true')}
              >
                <option value="">Use the global setting</option>
                <option value="true">Place on hold (loop guard enabled)</option>
                <option value="false">Never place on hold (loop guard disabled)</option>
              </select>
            </Field>
          {:else if tab === 'params'}
            {#if catalog}
              <ParamEditor {model} {catalog} />
            {:else}
              <p class="muted">Reading the engine parameters.</p>
            {/if}
          {:else if tab === 'speculative'}
            {#if catalog}
              <SpeculativeTab {model} {base} {catalog} />
            {:else}
              <p class="muted">Reading the engine parameters.</p>
            {/if}
          {:else if tab === 'advanced'}
            <Field label="Additional Arguments" help="Appended to the command line exactly as written, one per line, after the settings above. Parameters managed by the application are rejected.">
              <ListEditor bind:items={model.extra_args} placeholder="For example --override-kv" mono addLabel="Add Argument" />
            </Field>
            <Field label="Environment Variables" help="Applied only to the process of this model.">
              <div class="stack-sm">
                {#each Object.keys(model.env) as k (k)}
                  <div class="row">
                    <code class="mono">{k}</code>
                    <input type="text" class="mono grow" bind:value={model.env[k]} aria-label={`Value of ${k}`} />
                    <button class="quiet" onclick={() => delete model.env[k]}>Remove</button>
                  </div>
                {/each}
                <div class="row">
                  <input type="text" class="mono" placeholder="NAME" bind:value={envKey} aria-label="New variable name" />
                  <input type="text" class="mono grow" placeholder="value" bind:value={envValue} aria-label="New variable value" />
                  <button onclick={addEnv} disabled={!envKey.trim()}>Add Variable</button>
                </div>
              </div>
            </Field>
          {:else if tab === 'launch'}
            <h3>Launch Command<Hint text="The exact command line used to start this model. The port is selected at launch and the API key is hidden." label="About the launch command" /></h3>
            {#if previewError}<div class="notice error">{previewError}</div>{/if}
            {#if preview}
              <pre class="cmd mono">{preview.command}</pre>
              <button class="small" onclick={() => navigator.clipboard.writeText(preview?.command ?? '')}>Copy Command</button>
              {#if preview.warnings.length > 0}
                <div class="notice warn">
                  <strong>Skipped Settings</strong>
                  <ul>
                    {#each preview.warnings as w (w.key)}<li><span class="mono">{w.key}</span>: {w.message}</li>{/each}
                  </ul>
                </div>
              {/if}
              {#if preview.findings.length > 0}
                <div class="notice" class:warn={preview.findings.some((f) => f.severity !== 'info')}>
                  <strong>Speculative Decoding</strong>
                  <ul>
                    {#each preview.findings as f (f.message)}<li>{f.message}</li>{/each}
                  </ul>
                </div>
              {/if}
            {/if}
            <h3>Test Load<Hint text="Starts this model with its saved settings, reports how it loaded, and then unloads it. Save any changes first. The test does not run while a game is open." label="About the test load" /></h3>
            <div class="row">
              <button
                class="primary"
                disabled={testing || dirtyModel || runtime?.state === 'starting' || runtime?.state === 'ready'}
                onclick={runTest}
              >
                {testing ? 'Loading' : 'Test Load'}
              </button>
              {#if dirtyModel}<span class="muted">Save the changes to test them.</span>{/if}
              {#if runtime?.state === 'ready'}<span class="muted">This model is already loaded.</span>{/if}
            </div>
            {#if testError}<div class="notice error" role="alert">{testError}</div>{/if}
            {#if testReport}
              <div class="notice" class:warn={!testReport.ok} role="status">
                <strong>{testReport.ok ? 'Passed' : 'Requires attention'}.</strong>
                {testReport.message}
                <dl class="facts">
                  <dt>Load Time</dt><dd>{testReport.load_seconds.toFixed(1)} s</dd>
                  {#if testReport.gpu_layers}<dt>GPU Layers</dt><dd>{testReport.gpu_layers[0]} of {testReport.gpu_layers[1]}</dd>{/if}
                  {#if testReport.footprint_mib !== null}
                    <dt>GPU Memory</dt><dd>{mib(testReport.footprint_mib)}{testReport.footprint_estimated ? ' (estimated)' : ''}</dd>
                  {/if}
                  {#each testReport.speculative as s (s.spec_type)}
                    <dt>{typeInfo(s.spec_type).label}</dt>
                    <dd>
                      {s.confirmed ? 'Active' : 'Not Confirmed'}
                      {#if !s.confirmed}<Hint text="The engine did not report starting this type. A lowered log verbosity can hide the report, so this does not prove that the type is off." label="About Not Confirmed" />{/if}
                    </dd>
                  {/each}
                </dl>
              </div>
            {/if}
          {:else if tab === 'logs'}
            <h3>Log Output<Hint text="Output of the llama-server process of this model while it runs." label="About the log output" /></h3>
            <pre class="log mono">{logLines.length ? logLines.join('\n') : 'No output has been recorded. Load the model to view its log.'}</pre>
          {/if}
        </div>
      {/if}
    </section>
  </div>
{/if}

<style>
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
  .models {
    display: grid;
    grid-template-columns: 210px minmax(0, 1fr);
    gap: 24px;
    align-items: start;
  }
  .side {
    display: flex;
    flex-direction: column;
    gap: 14px;
    position: sticky;
    top: 0;
  }
  .picker {
    list-style: none;
    margin: 0;
    padding: 0;
    border-top: 1px solid var(--line);
  }
  .picker li {
    border-bottom: 1px solid var(--line);
  }
  .pick {
    width: 100%;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    border: none;
    border-radius: 0;
    background: transparent;
    padding: 8px 10px;
    text-align: left;
    border-left: 3px solid transparent;
  }
  .pick.on {
    background: var(--accent-soft);
    border-left-color: var(--accent);
  }
  .name {
    font-weight: 600;
  }
  .small {
    font-size: 12px;
  }
  .tabs {
    display: flex;
    gap: 2px;
    flex-wrap: wrap;
    border-bottom: 1px solid var(--line);
    margin: 14px 0 6px;
  }
  .tabs button {
    border: none;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    background: transparent;
    padding: 7px 12px;
    color: var(--ink-2);
  }
  .tabs button.on {
    border-bottom-color: var(--accent);
    color: var(--ink);
    font-weight: 600;
  }
  .panel {
    padding-top: 6px;
  }
  .sub {
    margin-top: 16px;
    padding-bottom: 4px;
    border-bottom: 1px solid var(--line);
  }
  .danger-zone {
    margin-top: 18px;
    padding-top: 12px;
    border-top: 1px solid var(--line);
    display: flex;
    gap: 10px;
    align-items: center;
    flex-wrap: wrap;
  }
  .cmd,
  .log {
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 10px 12px;
    white-space: pre-wrap;
    word-break: break-all;
    margin: 8px 0;
  }
  .log {
    max-height: 420px;
    overflow: auto;
    white-space: pre;
    word-break: normal;
  }
  @media (max-width: 860px) {
    .models {
      grid-template-columns: 1fr;
    }
    .side {
      position: static;
    }
  }
</style>
