<!--
  Toggle: an on/off switch with a text label.

  Props:  checked (bindable), label, disabled, hint (explanation shown on hover),
          reason (why the switch is unavailable; stays visible under the label)
  Emits:  nothing; use bind:checked.
  Calls:  no commands.
-->
<script lang="ts">
  import Hint from './Hint.svelte';

  let {
    checked = $bindable(false),
    label,
    disabled = false,
    hint = '',
    reason = '',
  }: { checked?: boolean; label: string; disabled?: boolean; hint?: string; reason?: string } = $props();
</script>

<div class="toggle-wrap">
  <label class="toggle" class:disabled>
    <input type="checkbox" role="switch" bind:checked {disabled} />
    <span class="track" aria-hidden="true"><span class="thumb"></span></span>
    <span class="text">
      <span>{label}</span>
      {#if reason}<span class="reason muted">{reason}</span>{/if}
    </span>
  </label>
  {#if hint}<Hint text={hint} label={`About ${label}`} />{/if}
</div>

<style>
  .toggle-wrap {
    display: inline-flex;
    align-items: flex-start;
  }
  .toggle-wrap :global(.hint) {
    margin-top: 2px;
  }
  .toggle {
    display: inline-flex;
    align-items: flex-start;
    gap: 10px;
    cursor: pointer;
    user-select: none;
  }
  .toggle.disabled {
    opacity: 0.55;
    cursor: default;
  }
  input {
    position: absolute;
    opacity: 0;
    width: 0;
    height: 0;
  }
  .track {
    flex: none;
    margin-top: 1px;
    width: 32px;
    height: 18px;
    border-radius: 9px;
    background: var(--line-strong);
    position: relative;
    transition: background 0.15s;
  }
  .thumb {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: #fff;
    transition: transform 0.15s;
  }
  input:checked + .track {
    background: var(--accent);
  }
  input:checked + .track .thumb {
    transform: translateX(14px);
  }
  input:focus-visible + .track {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .text {
    display: flex;
    flex-direction: column;
  }
  .reason {
    font-size: 12px;
  }
</style>
