<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type ScanReport, type SourceInfo, type Stats } from '../api';
  import { formatCount } from '../format';

  let { onStart }: { onStart: () => void } = $props();

  let sources = $state<SourceInfo[]>([]);
  let stats = $state<Stats | null>(null);
  let report = $state<ScanReport | null>(null);
  let scanning = $state(false);
  let progress = $state({ done: 0, total: 0 });
  let error = $state<string | null>(null);

  onMount(() => {
    refresh();
    const off = api.onEvent((e) => {
      if (e.type === 'scanProgress') progress = { done: e.done, total: e.total };
    });
    return () => void off.then((f) => f());
  });

  async function refresh() {
    [sources, stats] = await Promise.all([api.listSources(), api.stats()]);
  }

  async function addFolder() {
    error = null;
    try {
      const path = await api.pickSourceFolder();
      if (!path) return;
      await api.addFolder(path);
      await scan();
    } catch (e) {
      error = String(e);
    }
  }

  async function scan() {
    error = null;
    scanning = true;
    progress = { done: 0, total: 0 };
    try {
      report = await api.scan();
    } catch (e) {
      error = String(e);
    } finally {
      scanning = false;
      await refresh();
    }
  }

  async function remove(id: string) {
    await api.removeSource(id);
    await refresh();
  }
</script>

<main>
  <header>
    <h1>Camera <span>Tinder</span></h1>
    <p class="muted">Swipe through your shoot. Keep the good ones.</p>
  </header>

  <section class="panel">
    <div class="row">
      <h2>Folders & cards</h2>
      <div class="actions">
        {#if sources.length}
          <button onclick={scan} disabled={scanning}>Rescan</button>
        {/if}
        <button class="primary" onclick={addFolder} disabled={scanning}>+ Add folder or SD card</button>
      </div>
    </div>

    {#if sources.length === 0}
      <p class="muted empty">Point Camera Tinder at a folder of photos or a mounted SD card (e.g. <code>/Volumes/NIKON Z 6</code>).</p>
    {:else}
      <ul>
        {#each sources as s (s.id)}
          <li>
            <div>
              <strong>{s.name}</strong>
              <div class="muted path">{s.id}</div>
            </div>
            <span class="muted">{formatCount(s.photoCount)} photos</span>
            <button class="ghost" title="Remove from this session (files are not touched)" onclick={() => remove(s.id)} disabled={scanning}>✕</button>
          </li>
        {/each}
      </ul>
    {/if}

    {#if scanning}
      <div class="scan">
        <div class="muted">Scanning… {formatCount(progress.done)} / {formatCount(progress.total)}</div>
        <div class="progress"><div style:width="{progress.total ? (100 * progress.done) / progress.total : 0}%"></div></div>
      </div>
    {/if}

    {#if report && !scanning}
      <p class="muted report">
        Found {formatCount(report.found)} photos ({formatCount(report.new)} new).
        {#if report.skippedUnsupported}{formatCount(report.skippedUnsupported)} RAW files skipped for now; RAW culling is coming next.{/if}
        {#if report.failed.length}<span class="error">{report.failed.length} couldn't be read.</span>{/if}
      </p>
    {/if}
    {#if error}<p class="error">{error}</p>{/if}
  </section>

  {#if stats && stats.total > 0}
    <section class="panel start">
      <div>
        <div class="big">{formatCount(stats.undecided)}</div>
        <div class="muted">left to cull of {formatCount(stats.total)}</div>
      </div>
      <div class="tally">
        <span style:color="var(--like)">♥ {stats.like}</span>
        <span style:color="var(--favorite)">★ {stats.favorite}</span>
        <span style:color="var(--dislike)">✕ {stats.dislike}</span>
        <span style:color="var(--hate)">☠ {stats.hate}</span>
      </div>
      <button class="primary go" onclick={onStart} disabled={scanning}>Start swiping →</button>
    </section>
  {/if}
</main>

<style>
  main { max-width: 760px; margin: 0 auto; padding: 48px 24px; height: 100%; overflow-y: auto; }
  header { margin-bottom: 28px; }
  h1 { font-size: 34px; margin: 0 0 4px; letter-spacing: -0.02em; }
  h1 span { color: var(--accent); }
  h2 { font-size: 16px; margin: 0; }
  .panel { background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius); padding: 20px; margin-bottom: 16px; }
  .row { display: flex; justify-content: space-between; align-items: center; gap: 12px; flex-wrap: wrap; }
  .actions { display: flex; gap: 8px; }
  .empty { margin: 16px 0 0; }
  ul { list-style: none; padding: 0; margin: 16px 0 0; }
  li { display: grid; grid-template-columns: 1fr auto auto; gap: 12px; align-items: center; padding: 10px 0; border-top: 1px solid var(--border); }
  .path { font-size: 12px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 440px; }
  .scan { margin-top: 16px; display: grid; gap: 8px; }
  .report { margin: 14px 0 0; }
  .start { display: flex; align-items: center; gap: 24px; flex-wrap: wrap; }
  .big { font-size: 40px; font-weight: 700; letter-spacing: -0.02em; }
  .tally { display: flex; gap: 14px; font-weight: 600; }
  .go { margin-left: auto; padding: 12px 22px; font-size: 16px; }
  code { font-size: 13px; }
</style>
