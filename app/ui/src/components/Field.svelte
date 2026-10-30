<!--
  Field: a labelled setting whose explanation appears on hover, with its
  validation problem and a note about when a change takes effect.

  Props:  label, path (config path used for the problem and the apply note),
          help, children (the control)
  Emits:  nothing.
  Calls:  no commands.
-->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import { findingAt, isChangedAt } from '../lib/draft.svelte';
  import { SCOPE_LABEL, scopeOf } from '../lib/strings';
  import Hint from './Hint.svelte';

  let {
    label,
    path = '',
    help = '',
    children,
  }: { label: string; path?: string; help?: string; children: Snippet } = $props();

  const finding = $derived(path ? findingAt(path) : undefined);
  const changed = $derived(path ? isChangedAt(path) : false);
  const scope = $derived(path ? scopeOf(path) : 'live');
</script>

<div class="field">
  <div class="label">{label}{#if help}<Hint text={help} label={`About ${label}`} />{/if}</div>
  <div class="control">
    {@render children()}
    {#if finding}<p class={finding.severity === 'error' ? 'field-error' : finding.severity === 'warning' ? 'field-warning' : 'field-note'} role={finding.severity === 'error' ? 'alert' : 'status'}>{finding.message}</p>{/if}
    {#if changed && scope !== 'live'}<p class="scope">{SCOPE_LABEL[scope]}.</p>{/if}
  </div>
</div>

<style>
  .field {
    display: grid;
    grid-template-columns: minmax(140px, 190px) minmax(0, 1fr);
    gap: 4px 20px;
    padding: 10px 0;
    align-items: start;
  }
  .label {
    padding-top: 6px;
    color: var(--ink);
    font-weight: 500;
  }
  .control {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .scope {
    font-size: 12px;
    color: var(--drain);
  }
  @media (max-width: 700px) {
    .field {
      grid-template-columns: 1fr;
    }
    .label {
      padding-top: 0;
    }
  }
</style>
