<!--
  ListEditor: edits a list of strings, one per line, with add and remove.

  Props:  items (bindable string array), placeholder, addLabel, mono
  Emits:  nothing; use bind:items.
  Calls:  no commands.
-->
<script lang="ts">
  let {
    items = $bindable<string[]>([]),
    placeholder = '',
    addLabel = 'Add',
    mono = false,
  }: { items?: string[]; placeholder?: string; addLabel?: string; mono?: boolean } = $props();

  let fresh = $state('');

  function add(): void {
    const v = fresh.trim();
    if (v && !items.includes(v)) items = [...items, v];
    fresh = '';
  }
</script>

<div class="stack-sm">
  {#each items as item, i (i)}
    <div class="row">
      <input type="text" class:mono bind:value={items[i]} class="grow" />
      <button type="button" class="quiet" aria-label={`Remove ${item}`} onclick={() => (items = items.filter((_v, j) => j !== i))}>Remove</button>
    </div>
  {/each}
  <div class="row">
    <input
      type="text"
      class:mono
      class="grow"
      bind:value={fresh}
      {placeholder}
      onkeydown={(e) => {
        if (e.key === 'Enter') {
          e.preventDefault();
          add();
        }
      }}
    />
    <button type="button" onclick={add} disabled={!fresh.trim()}>{addLabel}</button>
  </div>
</div>
