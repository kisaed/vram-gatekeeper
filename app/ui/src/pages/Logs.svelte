<!--
  Logs page: the app's own log and each model's llama-server output.

  Props:  none
  Emits:  nothing
  Calls:  get_logs (history); live lines arrive as core events
-->
<script lang="ts">
  import { api, explain } from '../lib/api';
  import { draft } from '../lib/draft.svelte';
  import { followModelLog } from '../lib/live.svelte';
  import Hint from '../components/Hint.svelte';

  let source = $state('app');
  let lines = $state<string[]>([]);
  let filter = $state('');
  let follow = $state(true);
  let exported = $state('');
  let exportError = $state('');
  let box: HTMLPreElement | undefined = $state();

  const sources = $derived([
    { id: 'app', label: 'VRAM Gatekeeper' },
    ...(draft.saved?.models ?? []).map((m) => ({ id: `model:${m.id}`, label: m.display_name || m.id })),
  ]);

  $effect(() => {
    const current = source;
    if (current.startsWith('model:')) return followModelLog(current.slice('model:'.length), 600, 2000, (l) => (lines = l));
    api.getLogs(current, 600).then((l) => (lines = l)).catch(() => (lines = []));
  });

  const shown = $derived(
    filter.trim() ? lines.filter((l) => l.toLowerCase().includes(filter.trim().toLowerCase())) : lines,
  );

  $effect(() => {
    void shown.length;
    if (follow && box) box.scrollTop = box.scrollHeight;
  });

  async function exportBundle(): Promise<void> {
    exported = '';
    exportError = '';
    try {
      exported = await api.exportDiagnostics();
    } catch (e) {
      exportError = explain(e);
    }
  }
</script>

<div class="stack">
  <header class="spread">
    <h1>Logs</h1>
    <div class="row">
      <select bind:value={source} aria-label="Log source">
        {#each sources as s (s.id)}<option value={s.id}>{s.label}</option>{/each}
      </select>
      <input type="search" placeholder="Filter Lines" bind:value={filter} aria-label="Filter log lines" />
      <label class="row"><input type="checkbox" bind:checked={follow} /> Follow New Lines</label>
      <button onclick={() => navigator.clipboard.writeText(shown.join('\n'))}>Copy</button>
    </div>
  </header>
  <div class="row">
    <button onclick={exportBundle}>Export Diagnostics</button>
    <Hint text="Saves the logs, the settings without secrets, and system details into a single zip file. Nothing is transmitted." label="About diagnostics export" />
  </div>
  {#if exported}<div class="notice" role="status">Saved to <span class="mono">{exported}</span></div>{/if}
  {#if exportError}<div class="notice error" role="alert">{exportError}</div>{/if}
  <pre bind:this={box} class="log mono" aria-live="off">{shown.length ? shown.join('\n') : 'No log entries have been recorded.'}</pre>
</div>

<style>
  .log {
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 10px 12px;
    margin: 0;
    height: calc(100vh - 170px);
    min-height: 260px;
    overflow: auto;
    white-space: pre;
  }
</style>
