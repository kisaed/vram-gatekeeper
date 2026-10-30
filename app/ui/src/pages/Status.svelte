<!--
  Status page: is the gate open, how do clients connect, how full is the GPU, and
  what is each model doing right now. Opens with the gate banner (state, reason,
  the action that matters now), then the connection details, the memory map, the
  models, and the history.

  Props:  none
  Emits:  nothing
  Calls:  get_status (via live state), load_model, unload_model, restart_model,
          unload_all, allow_anyway, release_hold, set_paused
-->
<script lang="ts">
  import { api, explain } from '../lib/api';
  import { paramCatalog } from '../lib/catalog';
  import { draft } from '../lib/draft.svelte';
  import { gateColour, gateHeadline } from '../lib/gate';
  import { clock, idleFor, mib } from '../lib/format';
  import { live, refreshStatus } from '../lib/live.svelte';
  import { go, openModel } from '../lib/nav.svelte';
  import Hint from '../components/Hint.svelte';
  import VramBar from '../components/VramBar.svelte';
  import VramChart from '../components/VramChart.svelte';
  import StatusChip from '../components/StatusChip.svelte';

  const status = $derived(live.status);
  const startup = $derived(live.startup);
  const threshold = $derived(draft.saved?.vram.pressure.threshold_percent ?? 90);

  const headline = $derived(gateHeadline(status));
  /** A game has closed the gate and the user has not overridden it. */
  const gameBlocking = $derived(!!status?.session_active && !status.override_active);

  const subline = $derived.by(() => {
    if (!status) return '';
    const loaded = status.models.filter((m) => m.state === 'ready').length;
    const busy = status.models.reduce((n, m) => n + m.inflight, 0);
    return `${loaded} of ${status.models.length} models loaded; ${busy} active request${busy === 1 ? '' : 's'}.`;
  });

  const server = $derived(draft.saved?.server ?? null);
  /** Host and port clients connect to. A wildcard bind address is shown as the local loopback address. */
  const endpoint = $derived.by(() => {
    const addr = live.startup?.proxy_addr ?? (server ? `${server.host}:${server.port}` : null);
    return addr ? addr.replace(/^(0\.0\.0\.0|\[::\])/, '127.0.0.1') : null;
  });
  const openAiUrl = $derived(endpoint ? `http://${endpoint}/v1` : '');
  const anthropicUrl = $derived(endpoint ? `http://${endpoint}` : '');
  const defaultModel = $derived(server && !server.require_model_field ? server.default_model : null);

  /** The value most recently copied, so its button can confirm for a moment. */
  let copied = $state('');
  async function copy(value: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(value);
      copied = value;
      setTimeout(() => (copied === value ? (copied = '') : undefined), 1800);
    } catch {
      copied = '';
    }
  }

  let actionError = $state('');
  /** Engine version for the first-run panel, or why there is none. */
  let engine = $state<{ ok: boolean; text: string } | null>(null);

  $effect(() => {
    if (!status || status.models.length > 0 || engine) return;
    paramCatalog()
      .then((c) => (engine = c.params.length > 0 ? { ok: true, text: c.engine_version } : { ok: false, text: 'The engine did not report any parameters.' }))
      .catch((e) => (engine = { ok: false, text: explain(e) }));
  });

  /** Runs a command, reports failure, and refreshes. */
  async function act(run: () => Promise<unknown>): Promise<void> {
    actionError = '';
    try {
      await run();
    } catch (e) {
      actionError = explain(e);
    }
    await refreshStatus();
  }
</script>

<div class="stack">
  <header class="gatebar" style:--gate={gateColour(status)}>
    <div class="what">
      <h1>{headline}</h1>
      <p class="muted">{subline}</p>
    </div>
    <div class="row">
      {#if gameBlocking}
        <button class="primary" onclick={() => act(() => api.allowAnyway())}>Override Until the Game Closes</button>
      {/if}
      {#if status?.paused}
        <button class="primary" onclick={() => act(() => api.setPaused(false))}>Resume</button>
      {:else}
        <button onclick={() => act(() => api.setPaused(true))} disabled={!status}>Pause Gatekeeper</button>
      {/if}
      <button onclick={() => act(() => api.unloadAll())} disabled={!status}>Unload All Models</button>
    </div>
  </header>

  {#if startup?.startup_error}
    <div class="notice error" role="alert"><strong>The gatekeeper failed to start.</strong> {startup.startup_error}</div>
  {/if}
  {#if startup && (startup.parse_error || startup.issues.length > 0)}
    <div class="notice warn" role="alert">
      The configuration file contains errors. The most recent valid settings remain in use ({startup.config_source}).
      {#if startup.parse_error}{startup.parse_error}{/if}
      <ul>
        {#each startup.issues as i (i.path)}<li><span class="mono">{i.path}</span>: {i.message}</li>{/each}
      </ul>
    </div>
  {/if}
  {#if live.configError}
    <div class="notice warn" role="alert">
      A change to the configuration file was not applied.
      {#if live.configError.parse_error}{live.configError.parse_error}{/if}
      <ul>
        {#each live.configError.issues as i (i.path)}<li><span class="mono">{i.path}</span>: {i.message}</li>{/each}
      </ul>
    </div>
  {/if}
  {#if actionError}<div class="notice error" role="alert">{actionError}</div>{/if}
  {#if live.problem && !live.connecting}<div class="notice error" role="alert">{live.problem}</div>{/if}

  <section class="section" aria-labelledby="conn">
    <h2 id="conn">Connection Details</h2>
    {#if endpoint}
      <dl class="conn">
        <dt>
          OpenAI-Compatible Base URL
          <Hint text="Enter this address as the API base URL in clients that use the OpenAI API. Requests such as chat completions are sent to this address followed by /chat/completions." label="About the OpenAI-compatible base URL" />
        </dt>
        <dd>
          <code class="mono">{openAiUrl}</code>
          <button class="small" onclick={() => copy(openAiUrl)}>{copied === openAiUrl ? 'Copied' : 'Copy'}</button>
        </dd>
        <dt>
          Anthropic-Compatible Base URL
          <Hint text="Enter this address as the base URL in clients that use the Anthropic API. The client appends /v1/messages automatically." label="About the Anthropic-compatible base URL" />
        </dt>
        <dd>
          <code class="mono">{anthropicUrl}</code>
          <button class="small" onclick={() => copy(anthropicUrl)}>{copied === anthropicUrl ? 'Copied' : 'Copy'}</button>
        </dd>
        <dt>
          API Key
          <Hint text="The API key is configured on the General page and is never displayed here." label="About the API key" />
        </dt>
        <dd>{server?.api_key_set ? 'Required. Enter the key configured on the General page.' : 'Not required. If a client requires a value, any text is accepted.'}</dd>
        {#if defaultModel}
          <dt>
            Default Model
            <Hint text="Requests that do not specify a model are routed to this model." label="About the default model" />
          </dt>
          <dd><code class="mono">{defaultModel}</code></dd>
        {/if}
      </dl>
    {:else}
      <p class="muted">The connection address is not yet available.</p>
    {/if}
  </section>

  <section class="section" aria-labelledby="mem">
    <h2 id="mem">GPU Memory</h2>
    <VramBar sample={status?.vram ?? null} models={status?.models ?? []} thresholdPercent={threshold} notice={live.startup?.gpu_notice} />
  </section>

  <section class="section" aria-labelledby="models">
    <h2 id="models">Models</h2>
    {#if status && status.models.length === 0}
      <div class="start">
        <p>
          <strong>Welcome to VRAM Gatekeeper.</strong> The application unloads AI models when a game requires the GPU.
          To begin, add a model.
        </p>
        <p class="muted">
          {#if engine?.ok}
            The bundled llama.cpp engine is operational ({engine.text}). No additional installation is required.
          {:else if engine}
            The llama.cpp engine is not ready: {engine.text}
            <button class="small" onclick={() => go('engine')}>Open Engine Settings</button>
          {:else}
            Verifying the llama.cpp engine.
          {/if}
        </p>
        <div class="row">
          <button class="primary" onclick={() => go('download')}>Download a Model</button>
          <button onclick={() => go('models')}>Add a Model from a File</button>
        </div>
      </div>
    {/if}
    <div class="list">
      {#each status?.models ?? [] as m (m.id)}
        <div class="list-row model">
          <div class="who">
            <button class="link name" onclick={() => openModel(m.id)} title="Open the settings of this model">{m.display_name || m.id}</button>
            <span class="modelname muted">
              Model Name: <code class="mono">{m.id}</code>
              <button class="quiet small copy" onclick={() => copy(m.id)} aria-label={`Copy the model name ${m.id}`}>{copied === m.id ? 'Copied' : 'Copy'}</button>
              <Hint text="Enter this value in the model name field of the client. It identifies this model in every request, whether or not the model is currently loaded." label="About the model name" />
            </span>
          </div>
          <div class="state">
            <StatusChip state={m.state} cause={m.cause} />
            <div class="facts muted">
            {#if m.footprint_mib !== null}<span>{mib(m.footprint_mib)}{m.footprint_estimated ? ' (estimated)' : ''}</span>{/if}
            {#if m.gpu_layers}<span>{m.gpu_layers[0]} of {m.gpu_layers[1]} layers on the GPU</span>{/if}
            {#if m.inflight || m.queued}<span>{m.inflight} active{m.queued ? `, ${m.queued} queued` : ''}</span>{/if}
            {#if m.state === 'ready'}<span>{idleFor(m.idle_seconds)}</span>{/if}
            </div>
          </div>
          <div class="actions row">
            {#if m.state === 'unloaded' || m.state === 'failed'}
              <button class="small" onclick={() => act(() => api.loadModel(m.id))}>Load</button>
            {/if}
            {#if m.state === 'ready' || m.state === 'starting'}
              <button class="small" onclick={() => act(() => api.unloadModel(m.id, false))}>Unload</button>
              <button class="small" onclick={() => act(() => api.restartModel(m.id))}>Restart</button>
            {/if}
            {#if m.state !== 'held' && !m.manual_hold}
              <button class="small quiet" onclick={() => act(() => api.unloadModel(m.id, true))}>Unload and Hold</button>
            {/if}
            {#if m.state === 'held' || m.manual_hold}
              <button class="small" onclick={() => act(() => api.releaseHold(m.id))}>Release</button>
            {/if}
          </div>
          {#if m.failure}<p class="field-error full">{m.failure}</p>{/if}
          {#if m.overflow}
            <p class="notice warn full">Memory overflow into system RAM was detected: {m.overflow}. Performance is significantly lower than when the model remains in GPU memory.</p>
          {/if}
          {#if m.needs_restart}
            <p class="notice full">
              The settings of this model have changed since it was loaded. <button class="small" onclick={() => act(() => api.restartModel(m.id))}>Restart Now</button>
            </p>
          {/if}
        </div>
      {/each}
    </div>
  </section>

  <div class="pair">
    <section class="section" aria-labelledby="graph">
      <h2 id="graph">Last Five Minutes</h2>
      <VramChart points={live.history} thresholdPercent={threshold} />
    </section>

    <section class="section" aria-labelledby="events">
      <h2 id="events">Recent Events</h2>
      {#if live.feed.length === 0}
        <p class="muted">No events have been recorded. Game launches, unloads and warnings are listed here.</p>
      {:else}
        <ul class="feed">
          {#each live.feed as item (item.t + item.text)}
            <li class={item.kind}><span class="mono muted">{clock(item.t)}</span> {item.text}</li>
          {/each}
        </ul>
      {/if}
    </section>
  </div>
</div>

<style>
  /* The gate banner: the one place the page is tinted, so the state is the first thing seen. */
  .gatebar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px 20px;
    flex-wrap: wrap;
    padding: 14px 18px;
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--gate) 9%, var(--surface));
    border: 1px solid color-mix(in srgb, var(--gate) 35%, var(--line));
    border-left: 5px solid var(--gate);
  }
  .what {
    min-width: 0;
  }
  .conn {
    display: grid;
    grid-template-columns: max-content minmax(0, 1fr);
    gap: 8px 24px;
    align-items: center;
    margin: 0;
  }
  .conn dt {
    color: var(--ink-2);
    font-weight: 500;
  }
  .conn dd {
    margin: 0;
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .modelname {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
    font-size: 12px;
  }
  .modelname code {
    color: var(--ink-2);
    overflow-wrap: anywhere;
  }
  .copy {
    min-height: 0;
    padding: 0 6px;
  }
  .start {
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-width: 62ch;
    padding: 6px 0 12px;
  }
  .start p {
    margin: 0;
  }
  .model {
    display: grid;
    grid-template-columns: minmax(0, 2.3fr) minmax(0, 1fr) auto;
    grid-template-areas: 'who state actions';
    gap: 4px 20px;
    align-items: center;
  }
  .who {
    grid-area: who;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .state {
    grid-area: state;
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: 2px 16px;
  }
  .actions {
    grid-area: actions;
    justify-content: flex-end;
    flex-wrap: nowrap;
  }
  .full {
    grid-column: 1 / -1;
  }
  .link.name {
    align-self: flex-start;
    padding: 0;
    min-height: 0;
    border: none;
    background: transparent;
    font-weight: 600;
    text-align: left;
    color: var(--ink);
  }
  .link.name:hover:not(:disabled) {
    background: transparent;
    text-decoration: underline;
  }
  .pair {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(340px, 1fr));
    gap: 16px 28px;
    align-items: start;
  }
  .feed {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    max-height: 210px;
    overflow-y: auto;
  }
  .feed .warn {
    color: var(--warn-ink);
  }
  .feed .error {
    color: var(--closed);
  }
  @media (max-width: 1040px) {
    .model {
      grid-template-columns: minmax(0, 1fr) auto;
      grid-template-areas:
        'who actions'
        'state state';
    }
  }
</style>
