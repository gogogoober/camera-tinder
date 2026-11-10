<script lang="ts">
  import type { Decision } from '../api';
  import { DECISIONS } from '../decisions';

  let { decision }: { decision: Decision | null } = $props();
</script>

{#if decision}
  <em class="badge {decision}" title={DECISIONS[decision].label}>{DECISIONS[decision].icon}</em>
{/if}

<style>
  .badge {
    position: absolute;
    right: -4px;
    bottom: -4px;
    z-index: 1;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    font: 700 10px/1 system-ui, sans-serif;
    font-style: normal;
    color: #fff;
    border: 2px solid var(--bg);
    background: var(--c);
  }
  .like { --c: var(--like); }
  .dislike { --c: var(--dislike); }
  .favorite { --c: var(--favorite); color: #2b2100; }
  .hate { --c: var(--hate); }
</style>
