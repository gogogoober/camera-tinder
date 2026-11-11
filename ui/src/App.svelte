<script lang="ts">
  import { isMock, type Decision } from './lib/api';
  import Home from './lib/components/Home.svelte';
  import Cull from './lib/components/Cull.svelte';
  import Rescue from './lib/components/Rescue.svelte';
  import Review from './lib/components/Review.svelte';
  import ExportPanel from './lib/components/ExportPanel.svelte';
  import Offload from './lib/components/Offload.svelte';

  type View = 'home' | 'cull' | 'rescue' | 'review' | 'export' | 'offload';
  let view = $state<View>('home');
  /** The days picked on the Pick screen's calendar; Cull and Review show exactly these. */
  let days = $state<string[] | null>(null);
  /** What Review was showing when Export was opened: the decisions and days to start with. */
  let exportScope = $state<{ decisions: Decision[]; days: string[] | null } | null>(null);
  /** Set when a photo is reopened from Review. */
  let focusId = $state<number | null>(null);

  function start(picked: string[]) {
    days = picked;
    focusId = null;
    view = 'cull';
  }
</script>

{#if isMock}
  <div class="mock-badge" title="Running against the in-browser mock backend">MOCK DATA</div>
{/if}

{#if view === 'home'}
  <Home onStart={start} onOffload={() => (view = 'offload')} />
{:else if view === 'offload'}
  <Offload onBack={() => (view = 'home')} />
{:else if view === 'cull' && days}
  {#key focusId}
    <Cull {days} {focusId} onExit={() => (view = 'home')} onReview={() => (view = 'review')} onRescue={() => (view = 'rescue')} />
  {/key}
{:else if view === 'rescue' && days}
  <Rescue {days} onBack={() => (view = 'cull')} onDone={() => (view = 'review')} />
{:else if view === 'review'}
  <Review
    {days}
    onBack={() => (view = days ? 'cull' : 'home')}
    onOpen={(id) => { focusId = id; view = 'cull'; }}
    onExport={(decisions, scopeDays) => { exportScope = { decisions, days: scopeDays }; view = 'export'; }}
  />
{:else}
  <ExportPanel onBack={() => (view = 'review')} days={exportScope?.days ?? days} preset={exportScope?.decisions ?? null} />
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
    pointer-events: none;
  }
</style>
