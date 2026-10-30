<!--
  VramChart: the last five minutes of GPU memory use, drawn with uPlot.

  Props:  points (history), thresholdPercent
  Emits:  nothing.
  Calls:  no commands.
-->
<script lang="ts">
  import uPlot from 'uplot';
  import 'uplot/dist/uPlot.min.css';
  import type { VramPoint } from '../lib/live.svelte';

  let { points, thresholdPercent }: { points: VramPoint[]; thresholdPercent: number } = $props();

  /** Width of the graph. */
  const WINDOW_SECONDS = 300;

  let host: HTMLDivElement;
  let plot: uPlot | null = null;

  /** Reads a CSS custom property so the chart follows the theme. */
  function css(name: string): string {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  }

  function build(): uPlot {
    const grid = { stroke: css('--line'), width: 1 };
    const axis = { stroke: css('--ink-3'), grid, ticks: { stroke: css('--line'), width: 1 }, font: '11px "Segoe UI Variable Text", sans-serif' };
    return new uPlot(
      {
        width: host.clientWidth,
        height: 150,
        legend: { show: false },
        cursor: { show: false },
        scales: {
          // Always show the last five minutes, sliding with the clock, so a single
          // reading does not stretch the axis over years.
          x: {
            time: true,
            range: (): [number, number] => {
              const end = Date.now() / 1000;
              return [end - WINDOW_SECONDS, end];
            },
          },
          y: { range: (_u, _min, max) => [0, Math.max(1, Math.ceil(max))] },
        },
        axes: [axis, { ...axis, size: 58, values: (_u, v) => v.map((x) => `${x} GiB`) }],
        series: [
          {},
          { label: 'Used', stroke: css('--ink-2'), width: 1.5, fill: `${css('--ink-3')}33`, points: { show: false } },
          { label: 'Models', stroke: css('--accent'), width: 1.5, fill: `${css('--accent')}44`, points: { show: false } },
        ],
      },
      data(),
      host,
    );
  }

  function data(): uPlot.AlignedData {
    const t = points.map((p) => p.t / 1000);
    const gib = (v: number) => v / 1024;
    return [t, points.map((p) => gib(p.used)), points.map((p) => gib(p.models))];
  }

  $effect(() => {
    if (!host) return;
    plot ??= build();
    plot.setData(data());
  });

  $effect(() => {
    if (!host) return;
    const ro = new ResizeObserver(() => plot?.setSize({ width: host.clientWidth, height: 150 }));
    ro.observe(host);
    // Slide the window even when no new reading arrives.
    const slide = setInterval(() => plot?.redraw(), 1000);
    return () => {
      clearInterval(slide);
      ro.disconnect();
      plot?.destroy();
      plot = null;
    };
  });

  const last = $derived(points.at(-1));
</script>

<div class="chart">
  <div bind:this={host} class="host"></div>
  {#if points.length < 2}
    <p class="empty muted">The graph is populated as readings arrive, at one per second while a model is loaded.</p>
  {/if}
  {#if last && last.total > 0}
    <p class="muted caption">Pressure threshold: {thresholdPercent}% of {(last.total / 1024).toFixed(0)} GiB.</p>
  {/if}
</div>

<style>
  .chart {
    position: relative;
  }
  .host {
    width: 100%;
    min-height: 150px;
  }
  .empty {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    text-align: center;
    padding: 0 20px;
  }
  .caption {
    font-size: 12px;
    margin-top: 2px;
  }
</style>
