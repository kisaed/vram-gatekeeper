<!--
  StatusChip: a coloured dot plus words for a model state. Colour never
  carries the meaning alone.

  Props:  state (model state name), cause (optional unload cause)
  Emits:  nothing.
  Calls:  no commands.
-->
<script lang="ts">
  import { CAUSE_LABEL, STATE_LABEL } from '../lib/strings';
  import type { ModelStateName } from '../lib/types';

  let { state, cause = null }: { state: ModelStateName; cause?: string | null } = $props();

  const colour: Record<ModelStateName, string> = {
    unloaded: 'var(--paused)',
    starting: 'var(--busy)',
    ready: 'var(--open)',
    draining: 'var(--drain)',
    stopping: 'var(--drain)',
    held: 'var(--held)',
    failed: 'var(--closed)',
  };
</script>

<span class="chip">
  <span class="dot" class:pulse={state === 'starting'} style:background={colour[state]}></span>
  <span>{STATE_LABEL[state]}</span>
  {#if cause && (state === 'draining' || state === 'stopping')}
    <span class="muted">because {CAUSE_LABEL[cause] ?? cause}</span>
  {/if}
</span>

<style>
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    white-space: nowrap;
  }
  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    flex: none;
  }
  .pulse {
    animation: pulse 1.2s ease-in-out infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
</style>
