<!--
  SaveBar: appears while there are unsaved settings. Save validates and writes;
  Discard throws the edits away (spec §17.4).

  Props:  none
  Emits:  nothing
  Calls:  save_config (through the draft store); Ctrl+S does the same as Save changes
-->
<script lang="ts">
  import { discardDraft, dirtyPaths, draft, errorCount, saveDraft } from '../lib/draft.svelte';
  import { SCOPE_LABEL, scopeOf, type ApplyScope } from '../lib/strings';

  let saved = $state('');

  const paths = $derived(dirtyPaths());
  const problems = $derived(errorCount());

  /** Distinct "when does this apply" notes for the pending changes. */
  const scopes = $derived([...new Set(paths.map(scopeOf))].filter((s): s is Exclude<ApplyScope, 'live'> => s !== 'live'));

  async function save(): Promise<void> {
    const later = scopes;
    saved = '';
    if (await saveDraft()) {
      saved = later.length ? `Saved. ${later.map((s) => SCOPE_LABEL[s]).join('; ')}.` : 'Saved. The changes are in effect.';
      setTimeout(() => (saved = ''), 6000);
    }
  }

  /** Ctrl+S saves, unless there is nothing to save, a problem to fix, or a save already running. */
  function onkeydown(e: KeyboardEvent): void {
    if (!(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== 's') return;
    e.preventDefault();
    if (paths.length > 0 && problems === 0 && !draft.saving) void save();
  }
</script>

<svelte:window {onkeydown} />

{#if paths.length > 0 || saved}
  <div class="bar" role="region" aria-label="Unsaved settings">
    <div class="grow">
      {#if paths.length > 0}
        <strong>{paths.length} unsaved change{paths.length === 1 ? '' : 's'}.</strong>
        {#if problems > 0}
          <span class="bad">{problems} problem{problems === 1 ? '' : 's'} must be resolved before saving.</span>
        {:else if scopes.length > 0}
          <span class="muted">{scopes.map((s) => SCOPE_LABEL[s]).join('; ')}.</span>
        {/if}
        {#if draft.error}<span class="bad"> {draft.error}</span>{/if}
      {:else}
        <span role="status">{saved}</span>
      {/if}
    </div>
    {#if paths.length > 0}
      <button onclick={discardDraft}>Discard</button>
      <button class="primary" onclick={save} disabled={draft.saving} title="Ctrl+S">{draft.saving ? 'Saving' : 'Save Changes'}</button>
    {/if}
  </div>
{/if}

<style>
  .bar {
    position: sticky;
    bottom: 0;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 28px;
    background: var(--surface);
    border-top: 1px solid var(--line-strong);
    box-shadow: 0 -6px 16px -12px rgba(0, 0, 0, 0.4);
  }
  .bad {
    color: var(--closed);
  }
</style>
