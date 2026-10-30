<!--
  PathPicker: a text box for a file or folder path plus a Browse button that
  opens the native dialog.

  Props:  value (bindable; empty string means none), directory, filters,
          title, placeholder, invalid
  Emits:  nothing; use bind:value.
  Calls:  the dialog plugin (no app command).
-->
<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog';

  let {
    value = $bindable(''),
    directory = false,
    filters = [],
    title = 'Choose',
    placeholder = '',
    invalid = false,
  }: {
    value?: string;
    directory?: boolean;
    filters?: { name: string; extensions: string[] }[];
    title?: string;
    placeholder?: string;
    invalid?: boolean;
  } = $props();

  async function browse(): Promise<void> {
    const picked = await open({ directory, multiple: false, filters, title, defaultPath: value || undefined });
    if (typeof picked === 'string') value = picked;
  }
</script>

<span class="picker">
  <input type="text" class="mono grow" bind:value {placeholder} aria-invalid={invalid} />
  <button type="button" onclick={browse}>Browse</button>
</span>

<style>
  .picker {
    display: flex;
    gap: 6px;
    width: 100%;
  }
</style>
