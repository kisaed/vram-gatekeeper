<!--
  Download page: find GGUF models on Hugging Face, download them (pause,
  resume, verify), turn a finished download into a model, and manage the
  files in the models folder (spec §15, FR-MOD-4).

  Props:  none
  Emits:  nothing
  Calls:  hf_search, hf_repo, download_enqueue, download_pause, download_resume,
          download_cancel, download_dismiss, library_list, library_delete,
          add_downloaded_model, add_downloaded_draft, find_draft_models
-->
<script lang="ts">
  import { api, explain, isUiError } from '../lib/api';
  import { draft, loadDraft } from '../lib/draft.svelte';
  import { bytes, eta, percent, speed } from '../lib/format';
  import { live } from '../lib/live.svelte';
  import { go, openModel } from '../lib/nav.svelte';
  import Hint from '../components/Hint.svelte';
  import type { DownloadInfo, DraftSuggestion, Fit, LibraryFile, RepoFile, RepoSummary, RepoView, SearchSort, VariantView } from '../lib/types';

  let query = $state('');
  let sort = $state<SearchSort>('downloads');
  let results = $state<RepoSummary[] | null>(null);
  let searching = $state(false);
  let searchError = $state('');

  let selected = $state<RepoSummary | null>(null);
  let view = $state<RepoView | null>(null);
  let loadingRepo = $state(false);
  let repoError = $state('');
  let projector = $state('');
  let sidecar = $state('');

  // Find Draft Models (spec §10.6).
  let suggestions = $state<DraftSuggestion[] | null>(null);
  let suggesting = $state(false);
  let suggestError = $state('');

  // Add as a Draft Model: which finished download is being attached, and to which model.
  let attachFor = $state<number | null>(null);
  let attachModel = $state('');

  let message = $state('');
  let library = $state<LibraryFile[]>([]);
  let confirmDelete = $state<string | null>(null);
  let needsAccess = $state(false);

  // Search as the user types, after a short pause (spec §15.1).
  $effect(() => {
    const q = query.trim();
    const s = sort;
    if (q.length < 2) {
      results = null;
      return;
    }
    searching = true;
    const timer = setTimeout(() => {
      api
        .hfSearch(q, s)
        .then((r) => {
          results = r;
          searchError = '';
        })
        .catch((e) => (searchError = explain(e)))
        .finally(() => (searching = false));
    }, 400);
    return () => clearTimeout(timer);
  });

  async function openRepo(repo: RepoSummary): Promise<void> {
    selected = repo;
    view = null;
    repoError = '';
    needsAccess = false;
    projector = '';
    sidecar = '';
    suggestions = null;
    suggestError = '';
    loadingRepo = true;
    try {
      view = await api.hfRepo(repo.id);
    } catch (e) {
      repoError = explain(e);
      needsAccess = isUiError(e) && e.code === 'needs_access';
    } finally {
      loadingRepo = false;
    }
  }

  async function refreshLibrary(): Promise<void> {
    try {
      library = await api.libraryList();
    } catch {
      library = [];
    }
  }

  // Refresh the library on first view and whenever a download finishes.
  // The count is derived so progress ticks, which replace entries, don't re-run the scan.
  const doneCount = $derived(live.downloads.filter((d) => d.state === 'done').length);
  $effect(() => {
    void doneCount;
    void refreshLibrary();
  });

  const fitText: Record<Fit, string> = {
    fits_fully: 'Fits entirely in GPU memory',
    partial_cpu_offload: 'Exceeds GPU memory; part of the model runs on the CPU',
    too_big: 'Too large for acceptable performance',
  };
  const fitColour: Record<Fit, string> = {
    fits_fully: 'var(--open)',
    partial_cpu_offload: 'var(--drain)',
    too_big: 'var(--closed)',
  };

  async function download(v: VariantView): Promise<void> {
    if (!selected || !view) return;
    message = '';
    const files: RepoFile[] = [...v.files];
    const extra = view.mmproj.find((f) => f.path === projector);
    if (extra) files.push(extra);
    const side = view.sidecars.find((f) => f.path === sidecar);
    if (side) files.push(side);
    try {
      await api.downloadEnqueue({
        repo: selected.id,
        revision: 'main',
        commit_sha: view.sha,
        files,
        label: `${selected.id.split('/')[1] ?? selected.id} (${v.quant})`,
      });
      message = `Download started: ${v.quant} (${bytes(files.reduce((n, f) => n + f.size, 0))}).`;
    } catch (e) {
      message = explain(e);
    }
  }

  async function addModel(d: DownloadInfo): Promise<void> {
    message = '';
    try {
      const id = await api.addDownloadedModel(d.id);
      await loadDraft();
      await api.downloadDismiss(d.id);
      live.downloads = live.downloads.filter((x) => x.id !== d.id);
      openModel(id);
    } catch (e) {
      message = explain(e);
    }
  }

  async function findDrafts(): Promise<void> {
    if (!selected) return;
    suggesting = true;
    suggestError = '';
    try {
      suggestions = await api.findDraftModels(selected.id);
    } catch (e) {
      suggestError = explain(e);
    } finally {
      suggesting = false;
    }
  }

  const models = $derived(draft.value?.models ?? []);
  /** The model chosen for attaching, if it already has a draft file. */
  const replacing = $derived(models.find((m) => m.id === attachModel)?.files.draft ?? null);

  function startAttach(d: DownloadInfo): void {
    attachFor = d.id;
    attachModel = models[0]?.id ?? '';
  }

  async function attach(d: DownloadInfo): Promise<void> {
    message = '';
    try {
      const loaded = live.status?.models.find((m) => m.id === attachModel)?.state;
      await api.addDownloadedDraft(d.id, attachModel);
      await loadDraft();
      await api.downloadDismiss(d.id);
      live.downloads = live.downloads.filter((x) => x.id !== d.id);
      attachFor = null;
      const running = loaded !== undefined && loaded !== 'unloaded';
      message = `The file was attached to ${attachModel} as its draft file.${running ? ' It applies the next time the model loads.' : ''}`;
    } catch (e) {
      message = explain(e);
    }
  }

  async function removeFile(f: LibraryFile): Promise<void> {
    message = '';
    try {
      const n = await api.libraryDelete(f.path);
      message = `${n} file${n === 1 ? '' : 's'} deleted.`;
    } catch (e) {
      message = explain(e);
    }
    confirmDelete = null;
    await refreshLibrary();
  }

  const stateText: Record<string, string> = {
    queued: 'Queued',
    downloading: 'Downloading',
    paused: 'Paused',
    paused_for_game: 'Paused while a game is running',
    done: 'Completed and verified',
    failed: 'Failed',
    cancelled: 'Cancelled',
  };

  const active = $derived(live.downloads.filter((d) => d.state !== 'cancelled'));
  const shortName = (path: string) => path.split(/[\\/]/).slice(-2).join('/');
</script>

<div class="stack">
  <header>
    <h1>Download Models<Hint text="Search Hugging Face for GGUF models. Downloads resume after interruptions and are verified against the published checksum." label="About downloading models" /></h1>
  </header>

  {#if message}<div class="notice" role="status">{message}</div>{/if}

  {#if active.length > 0}
    <section class="section" aria-labelledby="queue">
      <h2 id="queue">Downloads</h2>
      <div class="list">
        {#each active as d (d.id)}
          <div class="list-row dl">
            <div class="head">
              <strong>{d.label}</strong>
              <span class="muted">{d.repo}</span>
            </div>
            <progress max="100" value={percent(d.bytes_done, d.bytes_total)} aria-label={`${d.label} progress`}></progress>
            <div class="meta row">
              <span>{stateText[d.state]}</span>
              <span class="muted">{bytes(d.bytes_done)} of {bytes(d.bytes_total)}</span>
              {#if d.state === 'downloading'}
                <span class="muted">{speed(d.speed_bps)} {eta(d.bytes_total - d.bytes_done, d.speed_bps)}</span>
              {/if}
              <span class="grow"></span>
              {#if d.state === 'downloading' || d.state === 'queued'}
                <button class="small" onclick={() => api.downloadPause(d.id)}>Pause</button>
              {/if}
              {#if d.state === 'paused' || d.state === 'failed' || d.state === 'paused_for_game'}
                <button class="small" onclick={() => api.downloadResume(d.id)}>{d.state === 'failed' ? 'Try Again' : 'Resume'}</button>
              {/if}
              {#if d.state !== 'done'}
                <button class="small quiet" onclick={() => api.downloadCancel(d.id)}>Cancel</button>
              {:else}
                <button class="small primary" onclick={() => addModel(d)}>Add as a Model</button>
                <button class="small" disabled={models.length === 0} onclick={() => startAttach(d)}>Add as a Draft Model</button>
                <button class="small quiet" onclick={() => api.downloadDismiss(d.id)}>Dismiss</button>
              {/if}
            </div>
            {#if attachFor === d.id}
              <div class="row attach">
                <label class="row">
                  Attach to
                  <select bind:value={attachModel} aria-label="Model that receives the draft file">
                    {#each models as m (m.id)}<option value={m.id}>{m.id}</option>{/each}
                  </select>
                </label>
                {#if replacing}<span class="field-warning">This model already has a draft file ({shortName(replacing)}); it will be replaced.</span>{/if}
                <button class="small primary" disabled={!attachModel} onclick={() => attach(d)}>Attach</button>
                <button class="small" onclick={() => (attachFor = null)}>Cancel</button>
              </div>
            {/if}
            {#if d.error}
              <p class="field-error" role="alert">{d.error}</p>
              {#if d.needs_access}<button class="small" onclick={() => go('general')}>Add an Access Token</button>{/if}
            {/if}
          </div>
        {/each}
      </div>
    </section>
  {/if}

  <section class="section" aria-labelledby="find">
    <h2 id="find">Find a Model</h2>
    <div class="row">
      <input type="search" class="grow" placeholder="Search, for example: qwen3 8b or llama 3.2" bind:value={query} aria-label="Search Hugging Face" />
      <select bind:value={sort} aria-label="Sort results">
        <option value="downloads">Most Downloaded</option>
        <option value="likes">Most Liked</option>
        <option value="updated">Recently Updated</option>
      </select>
    </div>
    {#if searchError}<div class="notice error" role="alert">{searchError}</div>{/if}

    <div class="browser">
      <div class="results">
        {#if results === null}
          <p class="muted">Enter at least two characters to search.</p>
        {:else if results.length === 0 && !searching}
          <p class="muted">No results match "{query.trim()}". Use fewer words.</p>
        {/if}
        <ul class="repos">
          {#each results ?? [] as r (r.id)}
            <li>
              <button class="repo" class:on={selected?.id === r.id} onclick={() => openRepo(r)}>
                <span class="rname">{r.id}</span>
                <span class="muted">{r.downloads.toLocaleString()} downloads, {r.likes.toLocaleString()} likes{r.gated ? ', access required' : ''}</span>
              </button>
            </li>
          {/each}
        </ul>
      </div>

      <div class="detail">
        {#if loadingRepo}
          <p class="muted">Reading the file list.</p>
        {:else if repoError}
          <div class="notice error" role="alert">
            {repoError}
            {#if needsAccess}<button class="small" onclick={() => go('general')}>Add an Access Token</button>{/if}
          </div>
        {:else if view && selected}
          <h3>{selected.id}</h3>
          <p class="muted">
            <a href={`https://huggingface.co/${selected.id}`} target="_blank" rel="noreferrer">Model Card and License on huggingface.co</a>
            {#if view.total_vram_mib}. File sizes are evaluated against the {Math.round(view.total_vram_mib / 1024)} GiB of GPU memory available.{/if}
          </p>
          {#if view.variants.length === 0}
            <p class="muted">This repository contains no GGUF files.</p>
          {/if}
          {#if view.mmproj.length > 0}
            <label class="row">
              Include a Vision Projector
              <select bind:value={projector}>
                <option value="">No</option>
                {#each view.mmproj as f (f.path)}<option value={f.path}>{f.path} ({bytes(f.size)})</option>{/each}
              </select>
            </label>
          {/if}
          {#if view.sidecars.length > 0}
            <label class="row">
              Include a Draft File
              <select bind:value={sidecar}>
                <option value="">No</option>
                {#each view.sidecars as f (f.path)}<option value={f.path}>{f.path} ({bytes(f.size)})</option>{/each}
              </select>
              <Hint text="Files named mtp-, dflash- or dspark- are draft files for speculative decoding. When this download is added as a model, the draft file is attached to it." label="About draft files" />
            </label>
          {/if}
          <div class="list">
            {#each view.variants as v (v.id)}
              <div class="list-row variant">
                <div>
                  <strong>{v.quant}</strong>
                  <span class="muted"> {bytes(v.total_bytes)}{v.kind === 'split' ? `, ${v.files.length} parts` : ''}</span>
                  <div class="fit"><span class="dot" style:background={fitColour[v.fit]}></span>{fitText[v.fit]}</div>
                  {#if !v.complete}<div class="field-error">Some parts are missing from this repository.</div>{/if}
                </div>
                <button class="small primary" disabled={!v.complete} onclick={() => download(v)}>Download</button>
              </div>
            {/each}
          </div>
          <div class="row">
            <button class="small" onclick={findDrafts} disabled={suggesting}>{suggesting ? 'Searching' : 'Find Draft Models'}</button>
            <Hint text="Searches Hugging Face for smaller models of the same family and for repositories whose names mention a draft. Nothing is contacted until you select this." label="About finding draft models" />
          </div>
          {#if suggestError}<div class="notice error" role="alert">{suggestError}</div>{/if}
          {#if suggestions}
            <h3>Suggested Draft Models</h3>
            <p class="muted">Compatibility is not guaranteed. The application checks the vocabulary once a file is attached to a model.</p>
            {#if suggestions.length === 0}<p class="muted">No suggestions were found.</p>{/if}
            <ul class="repos">
              {#each suggestions as s (s.repo.id)}
                <li>
                  <button class="repo" onclick={() => openRepo(s.repo)}>
                    <span class="rname">{s.repo.id}</span>
                    <span class="muted">{s.reason} {s.repo.downloads.toLocaleString()} downloads.</span>
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
        {:else}
          <p class="muted">Select a model on the left to view its available sizes.</p>
        {/if}
      </div>
    </div>
  </section>

  <section class="section" aria-labelledby="lib">
    <h2 id="lib">Models on This Computer</h2>
    {#if library.length === 0}
      <p class="muted">The models folder is empty. Downloaded files are listed here.</p>
    {/if}
    <div class="list">
      {#each library as f (f.path)}
        <div class="list-row row">
          <div class="grow">
            <div class="mono">{shortName(f.path)}</div>
            <div class="muted">
              {bytes(f.size)}, {f.quant}{f.is_projector ? ', vision projector' : ''}.
              {#if f.used_by.length > 0}Used by {f.used_by.join(', ')}.{:else}Not used by any model.{/if}
            </div>
          </div>
          {#if confirmDelete === f.path}
            <span>Delete this file?</span>
            <button class="small danger" onclick={() => removeFile(f)}>Delete</button>
            <button class="small" onclick={() => (confirmDelete = null)}>Cancel</button>
          {:else}
            <button class="small quiet" disabled={f.used_by.length > 0} title={f.used_by.length > 0 ? 'This file is in use by a model' : ''} onclick={() => (confirmDelete = f.path)}>Delete</button>
          {/if}
        </div>
      {/each}
    </div>
  </section>
</div>

<style>
  .dl {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .head {
    display: flex;
    gap: 10px;
    align-items: baseline;
    flex-wrap: wrap;
  }
  progress {
    width: 100%;
    height: 8px;
    accent-color: var(--accent);
  }
  .browser {
    display: grid;
    grid-template-columns: minmax(220px, 320px) minmax(0, 1fr);
    gap: 24px;
    margin-top: 10px;
    align-items: start;
  }
  .repos {
    list-style: none;
    margin: 0;
    padding: 0;
    border-top: 1px solid var(--line);
  }
  .repos li {
    border-bottom: 1px solid var(--line);
  }
  .repo {
    width: 100%;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    text-align: left;
    border: none;
    border-left: 3px solid transparent;
    border-radius: 0;
    background: transparent;
    padding: 8px 10px;
  }
  .repo.on {
    background: var(--accent-soft);
    border-left-color: var(--accent);
  }
  .rname {
    font-weight: 600;
    word-break: break-all;
  }
  .detail {
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-width: 0;
  }
  .attach {
    flex-wrap: wrap;
    padding: 6px 0;
  }
  .variant {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
  }
  .fit {
    display: flex;
    align-items: center;
    gap: 7px;
    font-size: 12px;
    color: var(--ink-2);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }
  a {
    color: var(--accent);
  }
  @media (max-width: 820px) {
    .browser {
      grid-template-columns: 1fr;
    }
  }
</style>
