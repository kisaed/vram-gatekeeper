<!--
  NumberField: a numeric input with an optional unit.

  Props:  value (bindable), min, max, step, unit, nullable (empty means null),
          width, invalid
  Emits:  nothing; use bind:value.
  Calls:  no commands.

  The input shows `value` and reports edits through `oninput`. (Using
  bind:value on a number input hands back a number, not text, which is easy to
  mishandle; reading the DOM string here keeps partial input like "-" intact.)
-->
<script lang="ts">
  let {
    value = $bindable<number | null>(0),
    min,
    max,
    step = 1,
    unit = '',
    nullable = false,
    width = '110px',
    invalid = false,
  }: {
    value?: number | null;
    min?: number;
    max?: number;
    step?: number;
    unit?: string;
    nullable?: boolean;
    width?: string;
    invalid?: boolean;
  } = $props();

  function onInput(e: Event & { currentTarget: HTMLInputElement }): void {
    const text = e.currentTarget.value.trim();
    if (text === '') {
      // An empty box means "no value" only where that is allowed.
      if (nullable) value = null;
      return;
    }
    const n = Number(text);
    if (Number.isFinite(n)) value = n;
  }
</script>

<span class="number">
  <input type="number" value={value ?? ''} oninput={onInput} {min} {max} {step} style:width aria-invalid={invalid} />
  {#if unit}<span class="unit muted">{unit}</span>{/if}
</span>

<style>
  .number {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
</style>
