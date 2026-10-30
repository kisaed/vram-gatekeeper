<!--
  App shell: the navigation rail (with the gate in one live sentence), the
  current page, and the save bar for settings.

  Props:  none
  Emits:  nothing
  Calls:  get_config (reloads), the window API (close confirmation)
-->
<script lang="ts">
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { onMount } from 'svelte';
  import { api } from './lib/api';
  import { dirtyPaths, discardDraft, draft, draftSnapshot, isDirty, loadDraft, validateDraft } from './lib/draft.svelte';
  import { clone } from './lib/format';
  import { gateColour, gateHeadline } from './lib/gate';
  import { live, startLive } from './lib/live.svelte';
  import { nav } from './lib/nav.svelte';
  import { PAGE_GROUPS, PAGE_LABEL, pageOfPath } from './lib/strings';
  import SaveBar from './components/SaveBar.svelte';
  import Download from './pages/Download.svelte';
  import Engine from './pages/Engine.svelte';
  import Games from './pages/Games.svelte';
  import General from './pages/General.svelte';
  import Logs from './pages/Logs.svelte';
  import Models from './pages/Models.svelte';
  import Status from './pages/Status.svelte';
  import VramRequests from './pages/VramRequests.svelte';

  let externalChange = $state(false);
  let confirmClose = $state(false);

  onMount(() => {
    void startLive();
    void loadDraft();
    // Closing with unsaved settings asks first.
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onCloseRequested((event) => {
        if (isDirty()) {
          event.preventDefault();
          confirmClose = true;
        }
      })
      .then((u) => (unlisten = u))
      .catch(() => {});
    return () => unlisten?.();
  });

  // Theme: follow Windows unless the user chose one.
  $effect(() => {
    const theme = draft.saved?.ui.theme ?? 'system';
    if (theme === 'system') delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = theme;
  });

  // Live validation of the draft while it has edits.
  $effect(() => {
    void draftSnapshot();
    if (!isDirty()) return;
    const timer = setTimeout(() => void validateDraft(), 300);
    return () => clearTimeout(timer);
  });

  // config.json changed on disk (hand edit, or our own save).
  $effect(() => {
    if (live.configReloads === 0) return;
    void (async () => {
      try {
        const disk = await api.getConfig();
        if (JSON.stringify(disk) === JSON.stringify(draft.saved)) return;
        if (isDirty()) {
          externalChange = true;
        } else {
          draft.saved = disk;
          draft.value = clone(disk);
        }
      } catch {
        /* the status page reports connection problems */
      }
    })();
  });

  const headline = $derived(gateHeadline(live.status));

  /** Pages holding unsaved edits, so the rail can say where they are. */
  const editedPages = $derived(new Set(dirtyPaths().map(pageOfPath)));
</script>

<div class="shell">
  <nav class="rail" aria-label="Pages">
    <div class="brand">VRAM Gatekeeper</div>
    <button class="gate" style:--gate={gateColour(live.status)} onclick={() => (nav.page = 'status')} aria-live="polite">
      <span class="dot"></span>
      <span>{headline}</span>
    </button>
    {#each PAGE_GROUPS as group (group.label)}
      <div class="group" role="group" aria-label={group.label}>
        <div class="group-label muted" aria-hidden="true">{group.label}</div>
        <ul>
          {#each group.pages as id (id)}
            <li>
              <button class="nav" class:on={nav.page === id} aria-current={nav.page === id ? 'page' : undefined} onclick={() => (nav.page = id)}>
                <span>{PAGE_LABEL[id]}</span>
                {#if editedPages.has(id)}<span class="edited" title="Unsaved Changes" role="img" aria-label="has unsaved changes"></span>{/if}
              </button>
            </li>
          {/each}
        </ul>
      </div>
    {/each}
    <div class="foot muted">
      {#if live.startup}Version {live.startup.version}{/if}
    </div>
  </nav>

  <div class="main">
    <main class="page">
      {#if externalChange}
        <div class="notice warn banner" role="alert">
          The configuration file was changed on disk while edits were in progress.
          <button
            class="small"
            onclick={async () => {
              await loadDraft();
              externalChange = false;
            }}>Load the File and Discard My Edits</button
          >
          <button class="small" onclick={() => (externalChange = false)}>Continue Editing</button>
        </div>
      {/if}
      {#if draft.error && !draft.value}
        <div class="notice error" role="alert">{draft.error}</div>
      {/if}

      {#if nav.page === 'status'}<Status />
      {:else if nav.page === 'models'}<Models />
      {:else if nav.page === 'download'}<Download />
      {:else if nav.page === 'games'}<Games />
      {:else if nav.page === 'vram'}<VramRequests />
      {:else if nav.page === 'engine'}<Engine />
      {:else if nav.page === 'general'}<General />
      {:else if nav.page === 'logs'}<Logs />
      {/if}
    </main>
    <SaveBar />
  </div>

  {#if confirmClose}
    <div class="scrim" role="presentation">
      <div class="dialog" role="alertdialog" aria-labelledby="cc-title" aria-modal="true">
        <h2 id="cc-title">Close Without Saving?</h2>
        <p>Changes to the settings have not been saved.</p>
        <div class="row end">
          <button onclick={() => (confirmClose = false)}>Continue Editing</button>
          <button
            class="danger"
            onclick={() => {
              discardDraft();
              void getCurrentWindow().destroy();
            }}>Discard and Close</button
          >
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .shell {
    display: grid;
    grid-template-columns: 216px minmax(0, 1fr);
    height: 100%;
  }
  .rail {
    display: flex;
    flex-direction: column;
    gap: 16px;
    padding: 18px 12px;
    background: var(--surface);
    border-right: 1px solid var(--line);
    overflow-y: auto;
  }
  .brand {
    font-family: var(--font-display);
    font-weight: 700;
    font-size: 15px;
    letter-spacing: -0.01em;
    padding: 0 8px;
  }
  /* The gate pill: tinted with the gate colour, and always with words. */
  .gate {
    display: flex;
    gap: 10px;
    align-items: flex-start;
    width: 100%;
    padding: 9px 10px;
    text-align: left;
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--gate) 11%, var(--surface));
    border: 1px solid color-mix(in srgb, var(--gate) 40%, var(--line));
    border-left: 4px solid var(--gate);
    line-height: 1.35;
  }
  .gate:hover:not(:disabled) {
    background: color-mix(in srgb, var(--gate) 16%, var(--surface));
    border-color: color-mix(in srgb, var(--gate) 55%, var(--line));
  }
  .dot {
    flex: none;
    width: 10px;
    height: 10px;
    margin-top: 5px;
    border-radius: 50%;
    background: var(--gate);
  }
  .group {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .group-label {
    padding: 0 10px 2px;
    font-size: 12px;
    font-weight: 600;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .nav {
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    text-align: left;
    border: none;
    background: transparent;
    padding: 6px 10px;
    border-radius: var(--radius);
    color: var(--ink-2);
  }
  .edited {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--drain);
  }
  .nav:hover:not(:disabled) {
    background: var(--surface-2);
    border: none;
  }
  .nav.on {
    background: var(--accent-soft);
    color: var(--ink);
    font-weight: 600;
  }
  .foot {
    margin-top: auto;
    padding: 0 8px;
    font-size: 12px;
  }
  .main {
    display: flex;
    flex-direction: column;
    min-width: 0;
    height: 100%;
    overflow-y: auto;
  }
  .page {
    flex: 1;
    width: 100%;
    max-width: 980px;
    padding: 24px 28px 32px;
  }
  .banner {
    margin-bottom: 16px;
  }
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(10, 14, 20, 0.5);
    display: grid;
    place-items: center;
    z-index: 10;
  }
  .dialog {
    background: var(--surface);
    border: 1px solid var(--line-strong);
    border-radius: 10px;
    padding: 20px 22px;
    width: min(420px, 90vw);
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .end {
    justify-content: flex-end;
    margin-top: 6px;
  }
  @media (max-width: 780px) {
    .shell {
      grid-template-columns: 1fr;
      grid-template-rows: auto minmax(0, 1fr);
    }
    .rail {
      flex-direction: row;
      flex-wrap: wrap;
      border-right: none;
      border-bottom: 1px solid var(--line);
    }
    .rail ul {
      flex-direction: row;
      flex-wrap: wrap;
    }
  }
</style>
