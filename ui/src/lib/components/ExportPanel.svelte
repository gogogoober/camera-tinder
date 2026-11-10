<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type ExportReport, type Stats } from '../api';
  import { formatCount } from '../format';

  let { onBack }: { onBack: () => void } = $props();

  let stats = $state<Stats | null>(null);
  let includeLikes = $state(true);
  let includeFavorites = $state(true);
  let running = $state(false);
  let progress = $state({ done: 0, total: 0 });
  let report = $state<ExportReport | null>(null);
  let error = $state<string | null>(null);

  const count = $derived(stats ? (includeLikes ? stats.like : 0) + (includeFavorites ? stats.favorite : 0) : 0);

  onMount(() => {
    api.stats().then((s) => (stats = s));
    const off = api.onEvent((e) => {
      if (e.type === 'exportProgress') progress = { done: e.done, total: e.total };
    });
    return () => void off.then((f) => f());
  });

  async function run() {
    error = null;
    report = null;
    try {
      const dest = await api.pickExportFolder();
      if (!dest) return;
      running = true;
      progress = { done: 0, total: count };
      report = await api.exportKeepers(dest, { includeLikes, includeFavorites });
    } catch (e) {
      error = String(e);
    } finally {
      running = false;
    }
  }
</script>

<main>
  <button class="ghost back" onclick={onBack} disabled={running}>← Back to swiping</button>
  <h1>Export keepers</h1>
  <p class="muted">Copies your keepers to a folder. Originals are never moved or deleted, and running it again only copies what's new.</p>

  <section class="panel">
    <label>
      <input type="checkbox" bind:checked={includeFavorites} disabled={running} />
      <span style:color="var(--favorite)">★ Favorites</span>
      <span class="muted">{formatCount(stats?.favorite ?? 0)}</span>
    </label>
    <label>
      <input type="checkbox" bind:checked={includeLikes} disabled={running} />
      <span style:color="var(--like)">♥ Likes</span>
      <span class="muted">{formatCount(stats?.like ?? 0)}</span>
    </label>

    <button class="primary go" onclick={run} disabled={running || count === 0}>
      Choose folder & copy {formatCount(count)} photos
    </button>

    {#if running}
      <div class="progress"><div style:width="{progress.total ? (100 * progress.done) / progress.total : 0}%"></div></div>
      <div class="muted">{formatCount(progress.done)} / {formatCount(progress.total)}</div>
    {/if}

    {#if report}
      <div class="report">
        <strong>Done.</strong> Copied {formatCount(report.copied)} to <code>{report.destination}</code>.
        {#if report.alreadyThere}<span class="muted">{formatCount(report.alreadyThere)} were already there.</span>{/if}
        {#if report.failed.length}
          <div class="error">{report.failed.length} failed:</div>
          <ul class="error">
            {#each report.failed.slice(0, 20) as [path, why] (path)}<li>{path}: {why}</li>{/each}
          </ul>
        {/if}
      </div>
    {/if}
    {#if error}<p class="error">{error}</p>{/if}
  </section>

  <p class="muted soon">Coming in v2: send to Lightroom and more.</p>
</main>

<style>
  main { max-width: 640px; margin: 0 auto; padding: 32px 24px; height: 100%; overflow-y: auto; }
  .back { color: var(--muted); margin-bottom: 16px; }
  h1 { margin: 0 0 6px; }
  .panel { background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); padding: 20px; margin-top: 20px; display: grid; gap: 14px; }
  label { display: flex; gap: 10px; align-items: center; font-weight: 600; }
  label .muted { margin-left: auto; font-weight: 400; }
  input[type='checkbox'] { width: 18px; height: 18px; accent-color: var(--accent); }
  .go { padding: 12px; font-size: 16px; }
  .report { line-height: 1.6; user-select: text; -webkit-user-select: text; }
  code { font-size: 13px; word-break: break-all; }
  ul { margin: 4px 0 0; padding-left: 18px; font-size: 13px; }
  .soon { margin-top: 16px; font-size: 13px; }
</style>
