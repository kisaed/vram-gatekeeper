<!--
  VRAM and requests page: pressure, overflow, loop guard, make-room, draining,
  and how gated requests are answered (spec §13, §7.4, §8.5).

  Props:  none
  Emits:  nothing
  Calls:  list_gpu_adapters
-->
<script lang="ts">
  import { api, explain } from '../lib/api';
  import { draft } from '../lib/draft.svelte';
  import { mib } from '../lib/format';
  import type { AdapterInfo } from '../lib/types';
  import Field from '../components/Field.svelte';
  import Hint from '../components/Hint.svelte';
  import NumberField from '../components/NumberField.svelte';
  import Toggle from '../components/Toggle.svelte';

  const cfg = $derived(draft.value);
  let adapters = $state<AdapterInfo[]>([]);
  let adapterError = $state('');

  $effect(() => {
    api.listGpuAdapters().then((a) => (adapters = a)).catch((e) => (adapterError = explain(e)));
  });

  const vendor = (id: number) => (id === 0x10de ? 'NVIDIA' : id === 0x1002 ? 'AMD' : id === 0x8086 ? 'Intel' : 'Unknown vendor');
</script>

{#if !cfg}
  <p class="muted">Loading settings.</p>
{:else}
  <div class="stack">
    <header>
      <h1>VRAM and Requests<Hint text="Defines the behavior when GPU memory runs short, and how clients are informed that a model is unavailable." label="About this page" /></h1>
    </header>

    <section class="section">
      <h2>When Another Application Requires GPU Memory<Hint text="After a model is unloaded, the next request loads it again as usual." label="About GPU memory pressure" /></h2>
      <Field label="Response to Low GPU Memory" path="vram.pressure.enabled">
        <Toggle bind:checked={cfg.vram.pressure.enabled} label="Unload Models When Memory Is Nearly Full" />
      </Field>
      {#if cfg.vram.pressure.enabled}
        <Field label="Threshold" path="vram.pressure.threshold_percent" help="Windows begins using system RAM before GPU memory reads as completely full, so some headroom is recommended.">
          <NumberField bind:value={cfg.vram.pressure.threshold_percent} min={1} max={100} unit="% of GPU memory" />
        </Field>
        <Field label="Minimum Duration" path="vram.pressure.sustain_seconds" help="Short spikes, such as shader compilation, are ignored.">
          <NumberField bind:value={cfg.vram.pressure.sustain_seconds} min={0} unit="seconds" />
        </Field>
        <Field label="Models to Unload" path="vram.pressure.scope">
          <select bind:value={cfg.vram.pressure.scope}>
            <option value="lru_until_below">Least recently used first, one at a time, until sufficient memory is available</option>
            <option value="all">All models</option>
          </select>
        </Field>
      {/if}
    </section>

    <section class="section">
      <h2>When a Model Overflows into System RAM<Hint text="The graphics driver can move a model into system RAM when GPU memory is full, which makes it far slower instead of causing a failure. VRAM Gatekeeper detects this when llama.cpp places layers on the CPU at load time, or when the driver moves memory while the model is running." label="About memory overflow" /></h2>
      <Field label="Action" path="vram.overflow.action">
        <select bind:value={cfg.vram.overflow.action}>
          <option value="error_and_unload">Stop with an error and unload the model</option>
          <option value="keep_running">Continue running and display a warning</option>
        </select>
      </Field>
      <Field label="Overflow Conditions">
        <div class="stack-sm">
          <Toggle bind:checked={cfg.vram.overflow.check_layer_offload} label="Layers placed on the CPU at load time" hint="Not applied when an exact layer count has been configured." />
          <Toggle bind:checked={cfg.vram.overflow.check_driver_spill} label="Driver moved memory into system RAM" />
        </div>
      </Field>
      {#if cfg.vram.overflow.check_driver_spill}
        <Field label="Driver Overflow Size" path="vram.overflow.spill_threshold_mib">
          <NumberField bind:value={cfg.vram.overflow.spill_threshold_mib} min={1} unit="MiB" />
        </Field>
      {/if}
      <details>
        <summary>Alternative: Fail Instead of Slowing Down</summary>
        <p class="muted">
          In the NVIDIA Control Panel, set "CUDA - Sysmem Fallback Policy" to "Prefer No Sysmem Fallback". Loading then fails
          outright when memory is insufficient, instead of running slowly.
        </p>
      </details>
    </section>

    <section class="section">
      <h2>Repeated Unloads</h2>
      <Field label="Loop Guard" path="vram.loop_guard.enabled" help="Prevents a model from being loaded and unloaded repeatedly while GPU memory remains insufficient.">
        <Toggle bind:checked={cfg.vram.loop_guard.enabled} label="Place a Model on Hold After Repeated Unloads" />
      </Field>
      {#if cfg.vram.loop_guard.enabled}
        <Field label="Hold After" path="vram.loop_guard.max_unloads">
          <div class="row">
            <NumberField bind:value={cfg.vram.loop_guard.max_unloads} min={1} width="80px" />
            <span>unloads within</span>
            <NumberField bind:value={cfg.vram.loop_guard.window_minutes} min={1} unit="minutes" width="80px" />
          </div>
        </Field>
        <Field label="Release Condition" path="vram.loop_guard.reload_margin_mib" help="A model on hold is released once this amount of memory is free beyond what the model requires, for the duration stated. The model can also be released manually.">
          <div class="row">
            <NumberField bind:value={cfg.vram.loop_guard.reload_margin_mib} min={0} unit="MiB spare" width="100px" />
            <NumberField bind:value={cfg.vram.loop_guard.release_sustain_seconds} min={1} unit="seconds" width="80px" />
          </div>
        </Field>
      {/if}
    </section>

    <section class="section">
      <h2>Multiple Models</h2>
      <Field label="Make Room" path="vram.make_room.enabled" help="Models that are busy or pinned are never unloaded for this purpose.">
        <Toggle bind:checked={cfg.vram.make_room.enabled} label="Unload Idle Models to Fit a New Model" />
      </Field>
      {#if cfg.vram.make_room.enabled}
        <Field label="Reserved Memory" path="vram.make_room.margin_mib">
          <NumberField bind:value={cfg.vram.make_room.margin_mib} min={0} unit="MiB" />
        </Field>
      {/if}
    </section>

    <section class="section">
      <h2>Requests in Progress When a Model Must Unload<Hint text="A cancelled request receives a clear error. Memory overflow into system RAM always cancels requests immediately." label="About requests in progress" /></h2>
      <Field label="Handling" path="draining.policy">
        <select bind:value={cfg.draining.policy}>
          <option value="finish">Allow them to finish, regardless of duration</option>
          <option value="wait_then_cancel">Wait for a limited time, then cancel the remainder</option>
          <option value="cancel">Cancel them immediately</option>
        </select>
      </Field>
      {#if cfg.draining.policy === 'wait_then_cancel'}
        <Field label="Maximum Wait" path="draining.timeout_seconds">
          <NumberField bind:value={cfg.draining.timeout_seconds} min={1} unit="seconds" />
        </Field>
      {/if}
    </section>

    <section class="section">
      <h2>Reporting Unavailable Models</h2>
      <Field label="Status Code" path="errors.status_code" help="503 is the conventional response for a temporarily unavailable service.">
        <select bind:value={cfg.errors.status_code}>
          <option value={503}>503 Service Unavailable</option>
          <option value={429}>429 Too Many Requests</option>
          <option value={423}>423 Locked</option>
          <option value={409}>409 Conflict</option>
        </select>
      </Field>
      <Field label="Suggested Retry Delay" path="errors.retry_after_seconds">
        <NumberField bind:value={cfg.errors.retry_after_seconds} min={0} unit="seconds" />
      </Field>
      <Field label="Error Details" path="errors.include_details">
        <Toggle bind:checked={cfg.errors.include_details} label="Include a Machine-Readable Reason in Error Responses" />
      </Field>
    </section>

    <section class="section">
      <h2>GPU Monitoring<Hint text="Memory rules are not available on integrated GPUs because they share system memory. Game detection continues to work." label="About GPU monitoring" /></h2>
      {#if adapterError}<div class="notice error">{adapterError}</div>{/if}
      <Field label="GPU" path="engine.gpu_adapter_luid" help="The GPU used for memory readings. By default, the graphics card with the most memory is monitored.">
        <select
          value={cfg.engine.gpu_adapter_luid ?? ''}
          onchange={(e) => (cfg.engine.gpu_adapter_luid = e.currentTarget.value || null)}
        >
          <option value="">Select Automatically</option>
          {#each adapters.filter((a) => !a.software) as a (a.luid)}
            <option value={a.luid}>{a.name} ({vendor(a.vendor_id)}, {mib(a.dedicated_mib)}){a.integrated ? ', integrated' : ''}</option>
          {/each}
        </select>
      </Field>
    </section>
  </div>
{/if}
