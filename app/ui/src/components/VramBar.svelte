<!--
  VramBar: the GPU memory map. One long bar split into each loaded model,
  everything else that uses the GPU, and what is free, with a marker where the
  pressure threshold sits. This is the main thing on the Status page.

  Props:  sample (latest VRAM reading or null), models (model status list),
          thresholdPercent, notice (why monitoring is off, if it is)
  Emits:  nothing.
  Calls:  no commands.
-->
<script lang="ts">
  import { mib, percent } from '../lib/format';
  import type { ModelStatus, VramSample } from '../lib/types';

  let {
    sample,
    models,
    thresholdPercent,
    notice = null,
  }: { sample: VramSample | null; models: ModelStatus[]; thresholdPercent: number; notice?: string | null } = $props();

  /** One hue per model, cycling; chosen to stay apart in both themes. */
  const HUES = [212, 262, 172, 32, 330, 92];

  const parts = $derived.by(() => {
    if (!sample) return null;
    const total = sample.total_mib;
    const byPid = new Map(sample.per_process.map((p) => [p.pid, p.dedicated_mib]));
    const own = models
      .filter((m) => m.pid !== null && (byPid.get(m.pid) ?? 0) > 0)
      .map((m, i) => ({ id: m.id, name: m.display_name || m.id, mib: byPid.get(m.pid as number) ?? 0, hue: HUES[i % HUES.length] }));
    const ownSum = own.reduce((s, m) => s + m.mib, 0);
    const other = Math.max(0, sample.used_mib - ownSum);
    const free = Math.max(0, total - sample.used_mib);
    return { total, own, other, free, usedPercent: percent(sample.used_mib, total) };
  });

  const over = $derived(parts !== null && parts.usedPercent >= thresholdPercent);
</script>

{#if parts}
  <figure class="vram" aria-label="GPU memory use">
    <div class="bar" role="img" aria-label={`${mib(sample?.used_mib)} of ${mib(parts.total)} in use`}>
      {#each parts.own as m (m.id)}
        <div
          class="seg"
          style:width={`${percent(m.mib, parts.total)}%`}
          style:background={`hsl(${m.hue} 70% 52%)`}
          title={`${m.name}: ${mib(m.mib)}`}
        ></div>
      {/each}
      <div class="seg other" style:width={`${percent(parts.other, parts.total)}%`} title={`Other applications: ${mib(parts.other)}`}></div>
      <div class="marker" style:left={`${thresholdPercent}%`} title={`Pressure threshold: ${thresholdPercent}%`}></div>
    </div>
    <figcaption>
      <ul class="legend">
        {#each parts.own as m (m.id)}
          <li><span class="swatch" style:background={`hsl(${m.hue} 70% 52%)`}></span>{m.name} <span class="mono">{mib(m.mib)}</span></li>
        {/each}
        <li><span class="swatch other"></span>Other applications <span class="mono">{mib(parts.other)}</span></li>
        <li><span class="swatch free"></span>Free <span class="mono">{mib(parts.free)}</span></li>
      </ul>
      <p class="total" class:over>
        {mib(sample?.used_mib)} of {mib(parts.total)} in use ({Math.round(parts.usedPercent)}%).
        {#if over}The {thresholdPercent}% threshold is exceeded.{/if}
      </p>
    </figcaption>
  </figure>
{:else}
  <div class="notice">
    GPU memory is not being monitored.{#if notice} {notice}{/if}
    <span class="muted">Game detection remains available.</span>
  </div>
{/if}

<style>
  .vram {
    margin: 0;
  }
  .bar {
    position: relative;
    display: flex;
    height: 34px;
    border-radius: var(--radius);
    background: var(--surface-2);
    border: 1px solid var(--line-strong);
    overflow: hidden;
  }
  .seg {
    height: 100%;
    min-width: 0;
    transition: width 0.4s ease;
  }
  .seg + .seg {
    border-left: 1px solid var(--surface);
  }
  .other {
    background: repeating-linear-gradient(135deg, var(--ink-3) 0 5px, color-mix(in srgb, var(--ink-3) 70%, var(--surface)) 5px 10px);
    opacity: 0.7;
  }
  .marker {
    position: absolute;
    top: -1px;
    bottom: -1px;
    width: 2px;
    background: var(--closed);
    transform: translateX(-1px);
  }
  figcaption {
    margin-top: 8px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .legend {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 4px 18px;
  }
  .legend li {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .swatch {
    width: 10px;
    height: 10px;
    border-radius: 2px;
  }
  .swatch.other {
    background: var(--ink-3);
    opacity: 0.7;
  }
  .swatch.free {
    background: var(--surface-2);
    border: 1px solid var(--line-strong);
  }
  .total {
    color: var(--ink-2);
  }
  .total.over {
    color: var(--closed);
    font-weight: 600;
  }
</style>
