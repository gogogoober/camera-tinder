<script lang="ts">
  import { isMock } from './lib/api';
  import Home from './lib/components/Home.svelte';
  import Cull from './lib/components/Cull.svelte';
  import ExportPanel from './lib/components/ExportPanel.svelte';

  type View = 'home' | 'cull' | 'export';
  let view = $state<View>('home');
</script>

{#if isMock}
  <div class="mock-badge" title="Running against the in-browser mock backend">MOCK DATA</div>
{/if}

{#if view === 'home'}
  <Home onStart={() => (view = 'cull')} />
{:else if view === 'cull'}
  <Cull onExit={() => (view = 'home')} onExport={() => (view = 'export')} />
{:else}
  <ExportPanel onBack={() => (view = 'cull')} />
{/if}

<style>
  .mock-badge {
    position: fixed;
    right: 10px;
    bottom: 10px;
    z-index: 100;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.08em;
    padding: 3px 8px;
    border-radius: 6px;
    background: #ffb02033;
    color: #ffb020;
  }
</style>
