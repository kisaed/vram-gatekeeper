<!--
  Hint: an explanation that appears on hover or keyboard focus instead of taking
  space under the text it explains. Shown as a small "i" marker.

  Props:  text (the explanation; blank lines separate paragraphs), label (what the
          marker is for, read by screen readers)
  Emits:  nothing.
  Calls:  no commands.

  The tip is positioned with `fixed` so scrolling panels and narrow columns never
  clip it, and it flips above the marker when there is no room below.
-->
<script lang="ts">
  let { text, label = 'More information' }: { text: string; label?: string } = $props();

  const id = `tip-${Math.random().toString(36).slice(2, 9)}`;
  let open = $state(false);
  /** False until the tip has been measured, so it never flashes in the wrong place. */
  let placed = $state(false);
  let marker: HTMLButtonElement | undefined = $state();
  let tip: HTMLDivElement | undefined = $state();
  let pos = $state({ top: 0, left: 0 });

  /** Places the tip under the marker, kept inside the window, or above it when the bottom is too close. */
  function place(): void {
    if (!marker || !tip) return;
    const m = marker.getBoundingClientRect();
    const t = tip.getBoundingClientRect();
    const gap = 6;
    const left = Math.max(8, Math.min(m.left - 8, window.innerWidth - t.width - 8));
    const below = m.bottom + gap;
    const top = below + t.height > window.innerHeight - 8 ? Math.max(8, m.top - gap - t.height) : below;
    pos = { top, left };
    placed = true;
  }

  function show(): void {
    open = true;
    // Measure after the tip is in the document.
    requestAnimationFrame(place);
  }
  function hide(): void {
    open = false;
    placed = false;
  }
</script>

<span class="hint">
  <button
    bind:this={marker}
    type="button"
    class="marker"
    aria-label={label}
    aria-describedby={open ? id : undefined}
    onmouseenter={show}
    onmouseleave={hide}
    onfocus={show}
    onblur={hide}
    onkeydown={(e) => e.key === 'Escape' && hide()}>i</button
  >
  {#if open}
    <div bind:this={tip} {id} class="tip" role="tooltip" style:top={`${pos.top}px`} style:left={`${pos.left}px`} style:visibility={placed ? 'visible' : 'hidden'}>
      {#each text.split('\n') as line, i (i)}<p>{line}</p>{/each}
    </div>
  {/if}
</span>

<style>
  .hint {
    display: inline-flex;
    vertical-align: middle;
    margin-left: 6px;
  }
  /* Overrides the global button look: this is a marker, not an action. */
  .marker {
    all: unset;
    box-sizing: border-box;
    width: 15px;
    height: 15px;
    border-radius: 50%;
    border: 1px solid var(--line-strong);
    color: var(--ink-3);
    font: italic 600 10px/13px var(--font-display);
    text-align: center;
    cursor: help;
  }
  .marker:hover,
  .marker:focus-visible {
    color: var(--accent);
    border-color: var(--accent);
    background: var(--accent-soft);
  }
  .tip {
    position: fixed;
    z-index: 30;
    width: max-content;
    max-width: min(360px, calc(100vw - 16px));
    padding: 8px 11px;
    border-radius: var(--radius);
    background: var(--surface);
    color: var(--ink);
    border: 1px solid var(--line-strong);
    box-shadow: 0 6px 20px rgba(10, 14, 20, 0.2);
    font-size: 12.5px;
    font-weight: 400;
    line-height: 1.45;
    text-align: left;
    pointer-events: none;
  }
  .tip p + p {
    margin-top: 4px;
    color: var(--ink-2);
  }
</style>
