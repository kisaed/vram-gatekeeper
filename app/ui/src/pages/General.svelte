<!--
  General page: listen address, network access and API key, startup,
  notifications, theme, logging and tuning (spec §17.3).

  Props:  none
  Emits:  nothing
  Calls:  set_api_key, secret_is_set, set_secret, clear_secret, open_config_folder
-->
<script lang="ts">
  import { api, explain } from '../lib/api';
  import { draft, loadDraft } from '../lib/draft.svelte';
  import { live, refreshStartup } from '../lib/live.svelte';
  import Field from '../components/Field.svelte';
  import ListEditor from '../components/ListEditor.svelte';
  import NumberField from '../components/NumberField.svelte';
  import Toggle from '../components/Toggle.svelte';

  const cfg = $derived(draft.value);

  let newKey = $state('');
  let keyMessage = $state('');
  let hfToken = $state('');
  let hfSet = $state(false);
  let hfMessage = $state('');

  $effect(() => {
    api.secretIsSet('hf_token').then((v) => (hfSet = v)).catch(() => (hfSet = false));
  });

  function generateKey(): void {
    const bytes = crypto.getRandomValues(new Uint8Array(24));
    newKey = `gk-${[...bytes].map((b) => b.toString(16).padStart(2, '0')).join('')}`;
  }

  async function saveKey(): Promise<void> {
    keyMessage = '';
    try {
      await api.setApiKey(newKey);
      newKey = '';
      keyMessage = 'The API key was saved. Clients must now include it in every request.';
      await loadDraft();
      await refreshStartup();
    } catch (e) {
      keyMessage = explain(e);
    }
  }

  async function removeKey(): Promise<void> {
    keyMessage = '';
    try {
      await api.setApiKey(null);
      keyMessage = 'The API key was removed.';
      await loadDraft();
    } catch (e) {
      keyMessage = explain(e);
    }
  }

  async function saveToken(): Promise<void> {
    hfMessage = '';
    try {
      await api.setSecret('hf_token', hfToken);
      hfSet = true;
      hfToken = '';
      hfMessage = 'The token was saved in Windows Credential Manager.';
    } catch (e) {
      hfMessage = explain(e);
    }
  }

  async function clearToken(): Promise<void> {
    await api.clearSecret('hf_token');
    hfSet = false;
    hfMessage = 'The token was removed.';
  }

  const base = $derived(live.startup?.proxy_addr ? `http://${live.startup.proxy_addr}/v1` : null);
</script>

{#if !cfg}
  <p class="muted">Loading settings.</p>
{:else}
  <div class="stack">
    <header>
      <h1>General</h1>
    </header>

    <section class="section">
      <h2>Client Connections</h2>
      {#if base}
        <p>Clients connect to <code class="mono">{base}</code>. When no API key is configured, clients may send any value.</p>
      {/if}
      <Field label="Address" path="server.host" help="Network address on which VRAM Gatekeeper listens. The default value accepts connections from this computer only.">
        <input type="text" class="mono" bind:value={cfg.server.host} />
      </Field>
      <Field label="Port" path="server.port" help="TCP port on which VRAM Gatekeeper listens.">
        <NumberField bind:value={cfg.server.port} min={1} max={65535} />
      </Field>
      <Field label="Network Access" path="server.allow_lan" help="Allows other computers on the network to use the models. An API key is required.">
        <Toggle bind:checked={cfg.server.allow_lan} label="Allow Access from Other Devices on the Network" disabled={!cfg.server.api_key_set && !cfg.server.allow_lan} reason={cfg.server.api_key_set ? '' : 'Configure an API key below first.'} />
      </Field>
      {#if live.startup?.lan_bound === false && cfg.server.allow_lan}
        <div class="notice warn">The application is currently listening on this computer only. Restart the application to enable network access.</div>
      {/if}
      <Field label="API Key" help={cfg.server.api_key_set ? 'A key is configured. It is stored in Windows Credential Manager and never in the settings file.' : 'No key is configured, so any program on this computer can use the models.'}>
        <div class="stack-sm">
          <p class="muted">{cfg.server.api_key_set ? 'A key is configured.' : 'No key is configured.'}</p>
          <div class="row">
            <input type="password" class="mono grow" bind:value={newKey} placeholder="Paste or generate a key" autocomplete="off" aria-label="New API key" />
            <button onclick={generateKey}>Generate</button>
            <button class="primary" onclick={saveKey} disabled={!newKey}>Save Key</button>
            {#if cfg.server.api_key_set}<button class="danger" onclick={removeKey} disabled={cfg.server.allow_lan}>Remove Key</button>{/if}
          </div>
          {#if keyMessage}<p role="status">{keyMessage}</p>{/if}
          {#if newKey}<p class="muted">Record this key now. It cannot be displayed again after it is saved. <span class="mono">{newKey}</span></p>{/if}
        </div>
      </Field>
      <Field label="Administration Interface" path="server.admin_api_enabled" help="Allows scripts to check status and control models at /gatekeeper/v1.">
        <Toggle bind:checked={cfg.server.admin_api_enabled} label="Enable the Administration API" />
      </Field>
      <Field label="Requests Without a Model Name" path="server.require_model_field" help="A request that does not specify a model is routed to the default model, unless rejection is enabled.">
        <div class="stack-sm">
          <Toggle bind:checked={cfg.server.require_model_field} label="Reject Requests That Do Not Specify a Model" />
          <select
            value={cfg.server.default_model ?? ''}
            onchange={(e) => (cfg.server.default_model = e.currentTarget.value || null)}
            aria-label="Default model"
            disabled={cfg.server.require_model_field}
          >
            <option value="">No Default Model</option>
            {#each cfg.models as m (m.id)}<option value={m.id}>{m.display_name || m.id}</option>{/each}
          </select>
        </div>
      </Field>
      <Field label="Maximum Request Size" path="server.max_request_body_mib" help="Requests that include images for vision models can be large.">
        <NumberField bind:value={cfg.server.max_request_body_mib} min={1} unit="MiB" />
      </Field>
      <Field label="Allowed Web Origins" help="Browsers block requests from other websites by default. List the origins that are permitted, for example https://chat.example.com.">
        <ListEditor bind:items={cfg.server.cors_allowed_origins} placeholder="https://chat.example.com" mono />
      </Field>
      {#if cfg.server.allow_lan}
        <Field label="Additional Host Names" help="Names that other devices use to reach this computer, for example my-pc.">
          <ListEditor bind:items={cfg.server.allowed_host_names} placeholder="my-pc" mono />
        </Field>
      {/if}
    </section>

    <section class="section">
      <h2>Hugging Face</h2>
      <Field label="Access Token" help="Required only for gated or private models. The token is stored in Windows Credential Manager.">
        <div class="stack-sm">
          <div class="row">
            <input type="password" class="mono grow" bind:value={hfToken} placeholder={hfSet ? 'A token is stored' : 'hf_...'} autocomplete="off" aria-label="Hugging Face token" />
            <button class="primary" onclick={saveToken} disabled={!hfToken}>Save Token</button>
            {#if hfSet}<button class="danger" onclick={clearToken}>Remove Token</button>{/if}
          </div>
          {#if hfMessage}<p role="status">{hfMessage}</p>{/if}
        </div>
      </Field>
    </section>

    <section class="section">
      <h2>Application</h2>
      <Field label="Startup" path="ui.start_with_windows">
        <div class="stack-sm">
          <Toggle bind:checked={cfg.ui.start_with_windows} label="Start with Windows" />
          <Toggle bind:checked={cfg.ui.open_window_on_start} label="Open This Window at Startup" hint="Otherwise the application starts minimized to the system tray." />
        </div>
      </Field>
      <Field label="Theme" path="ui.theme">
        <select bind:value={cfg.ui.theme}>
          <option value="system">Match Windows</option>
          <option value="light">Light</option>
          <option value="dark">Dark</option>
        </select>
      </Field>
      <Field label="Notifications" help="Windows notifications are shown for the selected events.">
        <div class="stack-sm">
          <Toggle bind:checked={cfg.ui.notifications.game_gate} label="Game started or closed" />
          <Toggle bind:checked={cfg.ui.notifications.vram_unload} label="Model unloaded because GPU memory is low" />
          <Toggle bind:checked={cfg.ui.notifications.overflow} label="Model overflowed into system RAM" />
          <Toggle bind:checked={cfg.ui.notifications.loop_guard} label="Model placed on hold after repeated unloads" />
          <Toggle bind:checked={cfg.ui.notifications.load_failures} label="Model failed to load" />
          <Toggle bind:checked={cfg.ui.notifications.downloads} label="Download completed or failed" />
        </div>
      </Field>
    </section>

    <section class="section">
      <h2>Files and Logs</h2>
      <Field label="Settings File" help="The settings file may be edited manually. Valid changes are applied immediately; invalid changes are rejected and reported.">
        <div class="row">
          <code class="mono">{live.startup?.config_path ?? ''}</code>
          <button onclick={() => api.openConfigFolder()}>Open Folder</button>
        </div>
      </Field>
      <Field label="Log Detail Level" path="logging.level">
        <select bind:value={cfg.logging.level}>
          <option value="error">Errors Only</option>
          <option value="warn">Warnings</option>
          <option value="info">Standard</option>
          <option value="debug">Detailed</option>
          <option value="trace">All Messages</option>
        </select>
      </Field>
      <Field label="Log Retention" path="logging.retention_days">
        <NumberField bind:value={cfg.logging.retention_days} min={1} unit="days" />
      </Field>
    </section>

    <details class="section">
      <summary><strong>Advanced Tuning</strong> <span class="muted">Rarely requires changes</span></summary>
      <Field label="GPU Sampling Interval (Active)" path="advanced.gpu_sample_interval_ms_active" help="Interval between GPU readings while a model is loaded or a game is running.">
        <NumberField bind:value={cfg.advanced.gpu_sample_interval_ms_active} min={100} unit="ms" />
      </Field>
      <Field label="GPU Sampling Interval (Idle)" path="advanced.gpu_sample_interval_ms_idle">
        <NumberField bind:value={cfg.advanced.gpu_sample_interval_ms_idle} min={1000} unit="ms" />
      </Field>
      <Field label="Sampling While Idle" path="advanced.sample_when_idle" help="Disable this option to read the GPU only while a model is loaded.">
        <Toggle bind:checked={cfg.advanced.sample_when_idle} label="Update GPU Readings When No Model Is Loaded" />
      </Field>
      <Field label="Load Status Check Interval" path="advanced.health_poll_interval_ms">
        <NumberField bind:value={cfg.advanced.health_poll_interval_ms} min={50} unit="ms" />
      </Field>
      <Field label="Memory Release Timeout" path="advanced.stop_release_wait_ms" help="Longest wait after a model stops before it is reported as unloaded.">
        <NumberField bind:value={cfg.advanced.stop_release_wait_ms} min={0} unit="ms" />
      </Field>
      <Field label="Maximum Retry Delay After Load Failures" path="advanced.load_failure_backoff_max_seconds">
        <NumberField bind:value={cfg.advanced.load_failure_backoff_max_seconds} min={1} unit="seconds" />
      </Field>
    </details>
  </div>
{/if}
