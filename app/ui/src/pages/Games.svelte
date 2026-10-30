<!--
  Games page: how games are detected and which programs count as games
  (spec §11, FR-GAME-1, FR-GAME-8).

  Props:  none
  Emits:  nothing
  Calls:  list_running_processes, check_game_executable, default_browse_dir
          (the file dialog comes from the dialog plugin)
-->
<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog';
  import { api, explain } from '../lib/api';
  import { draft } from '../lib/draft.svelte';
  import { mib } from '../lib/format';
  import { live } from '../lib/live.svelte';
  import type { GameCheck, RunningProgram } from '../lib/types';
  import Field from '../components/Field.svelte';
  import Hint from '../components/Hint.svelte';
  import ListEditor from '../components/ListEditor.svelte';
  import NumberField from '../components/NumberField.svelte';
  import Toggle from '../components/Toggle.svelte';

  const games = $derived(draft.value?.games ?? null);

  let typed = $state('');
  let pending = $state<GameCheck[]>([]);
  let message = $state('');
  let running = $state<RunningProgram[] | null>(null);
  let runningFilter = $state('');
  let loadingRunning = $state(false);
  let steamIds = $state('');

  $effect(() => {
    if (games) steamIds = games.steam_ignored_app_ids.join(', ');
  });

  /** Checks an input and either adds it or queues it for confirmation. */
  async function consider(input: string): Promise<void> {
    if (!games) return;
    message = '';
    try {
      const check = await api.checkGameExecutable(input);
      if (!check.ok) {
        message = check.error ?? 'This entry cannot be added.';
      } else if (check.warnings.length > 0) {
        if (!pending.some((p) => p.name.toLowerCase() === check.name.toLowerCase())) pending = [...pending, check];
      } else {
        commit(check);
      }
    } catch (e) {
      message = explain(e);
    }
  }

  function commit(check: GameCheck): void {
    if (!games) return;
    games.executables.push({ name: check.name, path: check.path, enabled: true });
    pending = pending.filter((p) => p.name !== check.name);
  }

  async function browse(): Promise<void> {
    const picked = await open({
      multiple: true,
      title: 'Choose Game Programs',
      defaultPath: await api.defaultBrowseDir(),
      filters: [{ name: 'Programs', extensions: ['exe'] }],
    });
    const paths = Array.isArray(picked) ? picked : typeof picked === 'string' ? [picked] : [];
    for (const p of paths) await consider(p);
  }

  async function addTyped(): Promise<void> {
    if (!typed.trim()) return;
    await consider(typed);
    if (!message) typed = '';
  }

  async function loadRunning(): Promise<void> {
    loadingRunning = true;
    try {
      running = await api.listRunningProcesses();
    } catch (e) {
      message = explain(e);
    } finally {
      loadingRunning = false;
    }
  }

  const shownRunning = $derived(
    (running ?? []).filter((r) => {
      const q = runningFilter.trim().toLowerCase();
      return !q || r.name.toLowerCase().includes(q) || (r.title ?? '').toLowerCase().includes(q);
    }),
  );

  const onList = (name: string) => games?.executables.some((e) => e.name.toLowerCase() === name.toLowerCase()) ?? false;

  function applySteamIds(): void {
    if (!games) return;
    games.steam_ignored_app_ids = steamIds
      .split(/[,\s]+/)
      .map((s) => Number(s))
      .filter((n) => Number.isInteger(n) && n > 0);
  }
</script>

{#if !games}
  <p class="muted">Loading settings.</p>
{:else}
  <div class="stack">
    <header>
      <h1>Games</h1>
      <p class="muted">
        {#if live.status?.session_active}Currently detected: <strong>{live.status.game}</strong> ({live.status.game_source}).
        {:else}No game is currently detected.{/if}
      </p>
    </header>

    <section class="section">
      <h2>Detection</h2>
      <Field label="Game Detection" path="games.enabled">
        <Toggle bind:checked={games.enabled} label="Unload Models When a Game Is Running" />
      </Field>
      <Field label="Detection Methods" help="A game is detected when any enabled method reports one. A game is considered closed only after all methods agree and the cooldown period has elapsed.">
        <div class="stack-sm">
          <Toggle bind:checked={games.detect_process_list} label="A program on the game list is running" />
          <Toggle bind:checked={games.detect_foreground_changes} label="Respond immediately when the active window changes" />
          <Toggle bind:checked={games.detect_steam} label="Steam reports a running game" />
          <Toggle bind:checked={games.detect_exclusive_fullscreen} label="A program runs in exclusive full-screen mode" hint="Full-screen video playback is not treated as a game." />
          <Toggle bind:checked={games.heavy_gpu_heuristic.enabled} label="A program uses the GPU heavily" hint="Detects games that are not on the list, such as borderless-window games. This option is disabled by default because 3D editors and video tools can also trigger it." />
        </div>
      </Field>
      {#if games.heavy_gpu_heuristic.enabled}
        <Field label="Heavy GPU Use Criteria" path="games.heavy_gpu_heuristic.min_3d_utilization_percent" help="A program is treated as a game when it exceeds all three values for the stated duration.">
          <div class="row">
            <NumberField bind:value={games.heavy_gpu_heuristic.min_3d_utilization_percent} min={1} max={100} unit="% 3D load" />
            <NumberField bind:value={games.heavy_gpu_heuristic.min_dedicated_mib} min={0} unit="MiB of VRAM" width="110px" />
            <NumberField bind:value={games.heavy_gpu_heuristic.sustain_seconds} min={1} unit="seconds" />
          </div>
        </Field>
      {/if}
      <Field label="Scan Interval" path="games.scan_interval_seconds" help="Frequent scans have negligible cost. A change of the active window is detected immediately regardless of this interval.">
        <NumberField bind:value={games.scan_interval_seconds} min={1} unit="seconds" />
      </Field>
      <Field label="Cooldown After Game Closes" path="games.cooldown_seconds" help="Prevents models from reloading while a launcher restarts the game.">
        <NumberField bind:value={games.cooldown_seconds} min={0} unit="seconds" />
      </Field>
    </section>

    <section class="section">
      <h2>Game List<Hint text="Programs on this list are treated as games whenever they are running. Matching uses the file name only." label="About the game list" /></h2>

      {#if games.executables.length === 0}
        <p class="muted">No games are listed. Add the games that are played, or rely on Steam and full-screen detection.</p>
      {/if}
      <div class="list">
        {#each games.executables as g, i (i)}
          <div class="list-row row game">
            <Toggle bind:checked={g.enabled} label={g.name} reason={g.path ?? 'Added by name'} />
            <span class="grow"></span>
            <button class="quiet" onclick={() => games.executables.splice(i, 1)}>Remove</button>
          </div>
        {/each}
      </div>

      <div class="adders">
        <button class="primary" onclick={browse}>Browse for a Program</button>
        <button onclick={loadRunning} disabled={loadingRunning}>{running ? 'Refresh Running Programs' : 'Add from Running Programs'}</button>
        <span class="row">
          <input
            type="text"
            class="mono"
            placeholder="game.exe"
            bind:value={typed}
            aria-label="Program name"
            onkeydown={(e) => e.key === 'Enter' && addTyped()}
          />
          <button onclick={addTyped} disabled={!typed.trim()}>Add by Name</button>
        </span>
      </div>
      {#if message}<p class="field-error" role="alert">{message}</p>{/if}

      {#each pending as p (p.name)}
        <div class="notice warn confirm">
          <p><strong>{p.name}</strong>{p.path ? ` (${p.path})` : ''}</p>
          <ul>{#each p.warnings as w (w)}<li>{w}</li>{/each}</ul>
          <div class="row">
            <button onclick={() => commit(p)}>Add Anyway</button>
            <button class="quiet" onclick={() => (pending = pending.filter((x) => x !== p))}>Skip</button>
          </div>
        </div>
      {/each}

      {#if running}
        <div class="running">
          <div class="spread">
            <h3>Running Programs<Hint text="Start the game first, then select it here. Programs that use the most GPU memory are listed first." label="About running programs" /></h3>
            <input type="search" placeholder="Filter" bind:value={runningFilter} aria-label="Filter running programs" />
          </div>
          <div class="list scroll">
            {#each shownRunning as r (r.name)}
              <div class="list-row row">
                <div class="grow">
                  <strong>{r.name}</strong>
                  {#if r.title}<span class="muted"> {r.title}</span>{/if}
                </div>
                {#if r.gpu_mib > 0}<span class="muted">{mib(r.gpu_mib)} GPU</span>{/if}
                <button class="small" disabled={onList(r.name)} onclick={() => consider(r.name)}>{onList(r.name) ? 'Listed' : 'Add'}</button>
              </div>
            {/each}
          </div>
        </div>
      {/if}
    </section>

    <section class="section">
      <h2>Exceptions</h2>
      <Field label="Programs Never Treated as Games" help="Programs that are ignored even when they run in full-screen mode or use the GPU heavily, for example a video player.">
        <ListEditor bind:items={games.ignored_executables} placeholder="player.exe" mono />
      </Field>
      <Field label="Steam Applications to Ignore" help="Steam application identifiers of tools that are not games. Separate multiple identifiers with commas.">
        <input type="text" class="mono" bind:value={steamIds} onchange={applySteamIds} placeholder="228980, 250820" />
      </Field>
    </section>
  </div>
{/if}

<style>
  .adders {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
    align-items: center;
    margin-top: 10px;
  }
  .confirm {
    margin-top: 10px;
  }
  .confirm ul {
    margin: 6px 0 10px;
    padding-left: 18px;
  }
  .running {
    margin-top: 16px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .scroll {
    max-height: 320px;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 0 12px;
    background: var(--surface);
  }
</style>
