<!--
  Engine page: which llama.cpp build runs the models (spec §16).

  Props:  none
  Emits:  nothing
  Calls:  get_param_catalog, refresh_catalog
-->
<script lang="ts">
  import { api, explain } from '../lib/api';
  import { draft, isDirty, loadDraft } from '../lib/draft.svelte';
  import { invalidateCatalog, paramCatalog, regenerateCatalog } from '../lib/catalog';
  import { bytes, percent } from '../lib/format';
  import { live } from '../lib/live.svelte';
  import type { EngineBackend, EnginePlan, InstalledEngine, ParamCatalog, UpdateCheck } from '../lib/types';
  import Field from '../components/Field.svelte';
  import Hint from '../components/Hint.svelte';
  import PathPicker from '../components/PathPicker.svelte';

  const cfg = $derived(draft.value);
  let catalog = $state<ParamCatalog | null>(null);
  let busy = $state(false);
  let message = $state('');

  $effect(() => {
    paramCatalog().then((c) => (catalog = c)).catch(() => (catalog = null));
  });

  let check = $state<UpdateCheck | null>(null);
  let checking = $state(false);
  let checkError = $state('');
  const progress = $derived(live.engineInstall);
  /** True from pressing Download until the finished download has been switched to. */
  let awaitingInstall = false;
  let installed = $state<InstalledEngine[]>([]);
  let switchNote = $state('');
  let switchUnsupported = $state<[string, string, string][]>([]);

  $effect(() => {
    void refreshInstalled();
  });

  async function refreshInstalled(): Promise<void> {
    installed = await api.listInstalledEngines().catch(() => []);
  }

  async function checkForUpdate(backend?: EngineBackend): Promise<void> {
    checking = true;
    checkError = '';
    try {
      check = await api.checkEngineUpdate(backend);
    } catch (e) {
      check = null;
      checkError = explain(e);
    } finally {
      checking = false;
    }
  }

  const downloading = $derived(progress !== null && !progress.finished);

  async function download(plan: EnginePlan): Promise<void> {
    checkError = '';
    switchNote = '';
    try {
      awaitingInstall = true;
      await api.installEngine(plan.tag, plan.backend);
    } catch (e) {
      awaitingInstall = false;
      checkError = explain(e);
    }
  }

  // Progress arrives as events; when this page's download ends, list it and switch to it.
  $effect(() => {
    const p = progress;
    if (!p?.finished || !awaitingInstall) return;
    awaitingInstall = false;
    void refreshInstalled().then(() => (p.installed ? use(p.installed) : undefined));
  });

  /** Switches to a folder (or the bundled engine for null) and reloads settings. */
  async function use(dir: string | null): Promise<void> {
    checkError = '';
    try {
      const report = await api.switchEngine(dir);
      await loadDraft();
      invalidateCatalog();
      catalog = await paramCatalog();
      switchNote = `Now using ${report.engine_version} (${report.param_count} parameters).`;
      switchUnsupported = report.unsupported;
    } catch (e) {
      checkError = explain(e);
    }
  }

  const usingBundled = $derived(!cfg?.engine.llama_server_dir);

  async function regenerate(): Promise<void> {
    busy = true;
    message = '';
    try {
      catalog = await regenerateCatalog();
      message = `${catalog.params.length} parameters were read from engine ${catalog.engine_version}.`;
    } catch (e) {
      message = explain(e);
    } finally {
      busy = false;
    }
  }
</script>

{#if !cfg}
  <p class="muted">Loading settings.</p>
{:else}
  <div class="stack">
    <header>
      <h1>Engine<Hint text="The llama.cpp build that loads and runs the models." label="About the engine" /></h1>
    </header>

    <section class="section">
      <h2>Engine Selection</h2>
      <Field label="Engine Folder" path="engine.llama_server_dir" help="Leave empty to use the engine supplied with VRAM Gatekeeper. To use a custom build, select the folder that contains llama-server.exe.">
        <PathPicker
          value={cfg.engine.llama_server_dir ?? ''}
          directory
          title="Choose the llama.cpp Folder"
          placeholder="Using the bundled engine"
        />
      </Field>
      <div class="row">
        <button onclick={() => (cfg.engine.llama_server_dir = null)} disabled={usingBundled}>Use the Bundled Engine</button>
        <span class="muted">{usingBundled ? 'The bundled engine is in use.' : 'A custom build is in use.'}</span>
      </div>
      <Field label="GPU Device" path="engine.device" help="Passed to llama-server as the device option, for example CUDA0 or Vulkan0. Leave empty to allow llama.cpp to choose.">
        <input type="text" class="mono" value={cfg.engine.device ?? ''} onchange={(e) => (cfg.engine.device = e.currentTarget.value.trim() || null)} placeholder="Select automatically" />
      </Field>
    </section>

    <section class="section" aria-labelledby="upd">
      <h2 id="upd">Updates<Hint text="New llama.cpp builds add model support and performance improvements. Checking for updates contacts GitHub only when the button is selected. The bundled engine remains on disk, so it is always possible to return to it." label="About engine updates" /></h2>
      <div class="row">
        <button onclick={() => checkForUpdate()} disabled={checking || downloading || isDirty()}>
          {checking ? 'Checking' : 'Check for a Newer Engine'}
        </button>
        {#if isDirty()}<span class="muted">Save or discard the pending changes first.</span>{/if}
        <button onclick={() => use(null)} disabled={usingBundled || downloading || isDirty()}>Return to the Bundled Engine</button>
      </div>
      {#if checkError}<div class="notice error" role="alert">{checkError}</div>{/if}
      {#if check}
        {#if check.plan}
          <div class="notice" role="status">
            {#if check.newer}
              Build {check.latest_build} is available (installed: {check.current_build ?? 'unknown build'}).
            {:else}
              The newest build is installed ({check.current_build}).
            {/if}
            <div class="row">
              <button class="primary" onclick={() => check?.plan && download(check.plan)} disabled={downloading || isDirty()}>
                Download {check.plan.backend} Build ({bytes(check.plan.assets.reduce((n, a) => n + a.size, 0))})
              </button>
              {#each check.alternatives as alt (alt.backend)}
                <button onclick={() => download(alt)} disabled={downloading || isDirty()}>
                  {alt.backend} Instead ({bytes(alt.assets.reduce((n, a) => n + a.size, 0))})
                </button>
              {/each}
            </div>
          </div>
        {:else}
          <div class="notice warn">No recent release provides a build for this type of GPU.</div>
        {/if}
      {/if}
      {#if progress}
        <div class="stack" role="status">
          <progress max="100" value={percent(progress.bytes_done, progress.bytes_total)} aria-label="Engine download"></progress>
          <span class="muted">
            {progress.finished ? (progress.error ? 'Failed.' : 'Completed.') : progress.step}
            {bytes(progress.bytes_done)} of {bytes(progress.bytes_total)}
          </span>
          {#if progress.error}<div class="notice error" role="alert">{progress.error}</div>{/if}
        </div>
      {/if}
      {#if switchNote}<div class="notice" role="status">{switchNote}</div>{/if}
      {#if switchUnsupported.length > 0}
        <div class="notice warn">
          <strong>This engine does not support some model settings. They will be skipped:</strong>
          <ul>
            {#each switchUnsupported as [model, key, why] (model + key)}<li><span class="mono">{model}</span> {key}: {why}</li>{/each}
          </ul>
        </div>
      {/if}
      {#if installed.length > 0}
        <h3>Downloaded Engines</h3>
        <div class="list">
          {#each installed as e (e.path)}
            <div class="list-row row">
              <span class="grow mono">{e.name}</span>
              {#if cfg.engine.llama_server_dir === e.path}
                <span class="muted">In use</span>
              {:else}
                <button class="small" onclick={() => use(e.path)} disabled={downloading || isDirty()}>Use</button>
              {/if}
            </div>
          {/each}
        </div>
      {/if}
    </section>

    <section class="section">
      <h2>Supported Parameters<Hint text="The parameters are read from the help output of the engine, so the Models page always matches the installed engine. If the engine folder was changed, save the changes first and then read the parameters again." label="About supported parameters" /></h2>
      {#if catalog && catalog.params.length > 0}
        <p>Engine {catalog.engine_version}. {catalog.params.length} parameters are available for each model.</p>
      {:else}
        <div class="notice warn">
          The engine parameters could not be read. Verify that the selected folder contains llama-server.exe and, for CUDA builds,
          the matching CUDA runtime files.
        </div>
      {/if}
      <div class="row">
        <button onclick={regenerate} disabled={busy}>{busy ? 'Reading' : 'Read the Parameters Again'}</button>
        {#if message}<span role="status">{message}</span>{/if}
      </div>
    </section>
  </div>
{/if}

<style>
  progress {
    width: 100%;
    height: 8px;
    accent-color: var(--accent);
  }
</style>
