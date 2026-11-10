<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type DaySummary, type Decision, type Filter, type Photo, type Stats, type StatusFilter } from '../api';
  import { DECISIONS, ORDER, decisionForKey } from '../decisions';
  import { formatCount, formatDay, formatTime, previewEdge } from '../format';
  import SwipeCard from './SwipeCard.svelte';

  let { onExit, onExport }: { onExit: () => void; onExport: () => void } = $props();

  interface Ghost { key: number; photo: Photo; decision: Decision; x: number; y: number }

  const edge = previewEdge();
  const PRELOAD = 4;
  const STATUS_TABS: [StatusFilter, string][] = [
    ['undecided', 'To cull'],
    ['keepers', 'Keepers'],
    ['rejects', 'Rejects'],
    ['all', 'All'],
  ];

  let filter = $state<Filter>({ day: null, status: 'undecided' });
  let list = $state<Photo[]>([]);
  let index = $state(0);
  let days = $state<DaySummary[]>([]);
  let stats = $state<Stats | null>(null);
  let ghosts = $state<Ghost[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let ghostKey = 0;

  const current = $derived<Photo | null>(list[index] ?? null);
  const remaining = $derived(filter.status === 'undecided' ? list.length : list.length - index);
  const nextDay = $derived(days.find((d) => d.stats.undecided > 0 && d.day !== filter.day));

  // Decisions/undos run strictly in order so the backend undo stack stays correct.
  let chain: Promise<unknown> = Promise.resolve();
  function enqueue<T>(f: () => Promise<T>): Promise<T> {
    const p = chain.then(f);
    chain = p.catch((e) => (error = String(e)));
    return p;
  }

  onMount(() => {
    load();
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });

  async function load(focusId?: number) {
    loading = true;
    error = null;
    try {
      list = await api.queue(filter);
      index = focusId != null ? Math.max(0, list.findIndex((p) => p.id === focusId)) : 0;
      await refreshCounts();
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function refreshCounts() {
    [days, stats] = await Promise.all([api.days(), api.stats()]);
  }

  function setFilter(patch: Partial<Filter>) {
    filter = { ...filter, ...patch };
    load();
  }

  const preloaded = new Map<number, HTMLImageElement>();
  $effect(() => {
    // Warm the next few previews (the backend caches them on disk too).
    const upcoming = list.slice(index + 1, index + 1 + PRELOAD);
    for (const p of upcoming) {
      if (preloaded.has(p.id)) continue;
      const img = new Image();
      img.src = api.previewUrl(p.id, edge);
      preloaded.set(p.id, img);
    }
    if (preloaded.size > 40) {
      const keep = new Set(upcoming.map((p) => p.id));
      for (const id of preloaded.keys()) if (!keep.has(id)) preloaded.delete(id);
    }
  });

  function decide(decision: Decision, from = { x: 0, y: 0 }) {
    const photo = current;
    if (!photo) return;
    ghosts.push({ key: ghostKey++, photo: { ...photo }, decision, x: from.x, y: from.y });
    if (filter.status === 'undecided') {
      list.splice(index, 1);
    } else {
      list[index] = { ...photo, decision };
      index++;
    }
    enqueue(() => api.decide(photo.id, decision)).then(refreshCounts);
  }

  async function undo() {
    const photo = await enqueue(() => api.undo());
    if (!photo) return;
    const at = list.findIndex((p) => p.id === photo.id);
    if (filter.status === 'undecided') {
      if (at >= 0) list.splice(at, 1);
      list.splice(index, 0, photo);
    } else if (at >= 0) {
      list[at] = photo;
      index = at;
    } else {
      list.splice(index, 0, photo);
    }
    await refreshCounts();
  }

  function skip() {
    if (!current) return;
    if (filter.status === 'undecided') list.push(...list.splice(index, 1));
    else index++;
  }

  function onKey(e: KeyboardEvent) {
    if (e.target instanceof HTMLInputElement) return;
    const d = decisionForKey(e.key);
    if (d) {
      e.preventDefault();
      if (!e.repeat) decide(d);
    } else if (e.key === 'Backspace' || (e.key === 'z' && (e.metaKey || e.ctrlKey)) || e.key === 'u') {
      e.preventDefault();
      undo();
    } else if (e.key === ' ') {
      e.preventDefault();
      skip();
    } else if (e.key === 'Escape') {
      onExit();
    }
  }
</script>

<div class="layout">
  <aside>
    <button class="ghost back" onclick={onExit}>← Folders</button>

    <nav class="days">
      <button class:active={filter.day === null} onclick={() => setFilter({ day: null })}>
        <span>All days</span>
        {#if stats}<span class="count">{formatCount(stats.undecided)}</span>{/if}
      </button>
      {#each days as d (d.day)}
        <button class:active={filter.day === (d.day ?? 'unknown')} onclick={() => setFilter({ day: d.day ?? 'unknown' })}>
          <span>{formatDay(d.day)}</span>
          <span class="count">{d.stats.undecided ? formatCount(d.stats.undecided) : '✓'}</span>
          <div class="bar"><div style:width="{(100 * (d.stats.total - d.stats.undecided)) / Math.max(1, d.stats.total)}%"></div></div>
        </button>
      {/each}
    </nav>

    {#if stats}
      <div class="tally">
        <span style:color="var(--like)">♥ {stats.like}</span>
        <span style:color="var(--favorite)">★ {stats.favorite}</span>
        <span style:color="var(--dislike)">✕ {stats.dislike}</span>
        <span style:color="var(--hate)">☠ {stats.hate}</span>
      </div>
      <button class="primary export" onclick={onExport} disabled={stats.like + stats.favorite === 0}>
        Export {formatCount(stats.like + stats.favorite)} keepers
      </button>
    {/if}
  </aside>

  <section class="main">
    <div class="tabs">
      {#each STATUS_TABS as [status, label] (status)}
        <button class:active={filter.status === status} onclick={() => setFilter({ status })}>{label}</button>
      {/each}
      <span class="muted left">{formatCount(remaining)} {filter.status === 'undecided' ? 'left' : 'to review'}</span>
    </div>

    <div class="stage">
      {#each ghosts as g (g.key)}
        <div
          class="ghost-card fly-{g.decision}"
          style="--x0: {g.x}px; --y0: {g.y}px; --r0: {g.x / 22}deg"
          onanimationend={() => (ghosts = ghosts.filter((x) => x.key !== g.key))}
        >
          <img class="photo-fill" src={api.previewUrl(g.photo.id, edge)} alt="" />
          <div class="stamp {g.decision}">{DECISIONS[g.decision].stamp}</div>
        </div>
      {/each}

      {#if current}
        {#key current.id}
          <SwipeCard photo={current} {edge} onDecide={decide} />
        {/key}
      {:else if !loading}
        <div class="done">
          <div class="emoji">🎉</div>
          <h2>{filter.status === 'undecided' ? 'All caught up here' : 'Nothing more to review'}</h2>
          {#if nextDay && filter.status === 'undecided'}
            <button class="primary" onclick={() => setFilter({ day: nextDay.day ?? 'unknown' })}>
              Next: {formatDay(nextDay.day)} ({formatCount(nextDay.stats.undecided)} left)
            </button>
          {/if}
          {#if stats && stats.like + stats.favorite > 0}
            <button onclick={onExport}>Export keepers</button>
          {/if}
        </div>
      {/if}
      {#if error}<p class="error toast">{error}</p>{/if}
    </div>

    <footer>
      <div class="meta">
        {#if current}
          <strong>{current.fileName}</strong>
          <span class="muted">{formatDay(current.capturedAt?.slice(0, 10) ?? null)} · {formatTime(current.capturedAt)}{current.camera ? ` · ${current.camera}` : ''}</span>
          {#if current.decision}<span class="badge {current.decision}">{DECISIONS[current.decision].label}</span>{/if}
        {/if}
      </div>
      <div class="buttons">
        <button class="round" title="Undo (⌫)" onclick={undo}>↶</button>
        {#each ORDER as d (d)}
          <button class="round big {d}" title="{DECISIONS[d].label} ({DECISIONS[d].arrow})" onclick={() => decide(d)} disabled={!current}>
            {DECISIONS[d].icon}
          </button>
        {/each}
        <button class="round" title="Skip (space)" onclick={skip} disabled={!current}>⤼</button>
      </div>
      <div class="keys muted">
        <kbd>←</kbd> nope <kbd>→</kbd> like <kbd>↑</kbd> favorite <kbd>↓</kbd> hate <kbd>⌫</kbd> undo <kbd>space</kbd> skip
      </div>
    </footer>
  </section>
</div>

<style>
  .layout { display: grid; grid-template-columns: 230px 1fr; height: 100%; }
  aside { background: var(--panel); border-right: 1px solid var(--border); padding: 14px 12px; display: flex; flex-direction: column; gap: 12px; min-height: 0; }
  .back { justify-self: start; align-self: flex-start; color: var(--muted); }
  .days { display: flex; flex-direction: column; gap: 4px; overflow-y: auto; flex: 1; min-height: 0; }
  .days button { display: grid; grid-template-columns: 1fr auto; gap: 6px 8px; text-align: left; background: transparent; border-color: transparent; padding: 8px 10px; }
  .days button.active { background: var(--panel-2); border-color: var(--border); }
  .count { color: var(--muted); font-variant-numeric: tabular-nums; }
  .bar { grid-column: 1 / -1; height: 3px; background: var(--border); border-radius: 9px; overflow: hidden; }
  .bar > div { height: 100%; background: var(--like); }
  .tally { display: flex; justify-content: space-between; font-weight: 600; font-size: 14px; padding: 0 4px; }
  .export { width: 100%; }

  .main { display: grid; grid-template-rows: auto 1fr auto; min-width: 0; min-height: 0; }
  .tabs { display: flex; gap: 6px; padding: 12px 18px; align-items: center; }
  .tabs button { padding: 6px 12px; background: transparent; border-color: transparent; color: var(--muted); }
  .tabs button.active { background: var(--panel-2); border-color: var(--border); color: var(--text); }
  .left { margin-left: auto; font-variant-numeric: tabular-nums; }

  .stage { position: relative; margin: 0 24px; min-height: 0; overflow: visible; }

  .ghost-card { position: absolute; inset: 0; pointer-events: none; animation: 0.32s ease-in forwards; z-index: 2; }
  .fly-like { animation-name: fly-right; }
  .fly-dislike { animation-name: fly-left; }
  .fly-favorite { animation-name: fly-up; }
  .fly-hate { animation-name: fly-down; }
  @keyframes fly-right { from { transform: translate(var(--x0), var(--y0)) rotate(var(--r0)); } to { transform: translate(110vw, var(--y0)) rotate(24deg); opacity: 0.4; } }
  @keyframes fly-left { from { transform: translate(var(--x0), var(--y0)) rotate(var(--r0)); } to { transform: translate(-110vw, var(--y0)) rotate(-24deg); opacity: 0.4; } }
  @keyframes fly-up { from { transform: translate(var(--x0), var(--y0)) rotate(var(--r0)); } to { transform: translate(var(--x0), -110vh) scale(0.9); opacity: 0.4; } }
  @keyframes fly-down { from { transform: translate(var(--x0), var(--y0)) rotate(var(--r0)); } to { transform: translate(var(--x0), 110vh) scale(0.9); opacity: 0.4; } }

  .done { position: absolute; inset: 0; display: grid; place-content: center; justify-items: center; gap: 12px; text-align: center; }
  .done .emoji { font-size: 48px; }
  .done h2 { margin: 0 0 6px; }
  .toast { position: absolute; bottom: 8px; left: 50%; translate: -50% 0; background: var(--panel); padding: 6px 12px; border-radius: 8px; }

  footer { padding: 12px 18px 16px; display: grid; gap: 10px; justify-items: center; }
  .meta { display: flex; gap: 10px; align-items: center; min-height: 22px; font-size: 14px; flex-wrap: wrap; justify-content: center; }
  .badge { font-size: 12px; font-weight: 700; padding: 2px 8px; border-radius: 99px; background: var(--panel-2); }
  .badge.like { color: var(--like); }
  .badge.dislike { color: var(--dislike); }
  .badge.favorite { color: var(--favorite); }
  .badge.hate { color: var(--hate); }
  .buttons { display: flex; gap: 14px; align-items: center; }
  .round { width: 44px; height: 44px; border-radius: 50%; padding: 0; font-size: 18px; color: var(--muted); }
  .round.big { width: 60px; height: 60px; font-size: 26px; background: var(--panel); }
  .round.like { color: var(--like); }
  .round.dislike { color: var(--dislike); }
  .round.favorite { color: var(--favorite); }
  .round.hate { color: var(--hate); }
  .keys { font-size: 12px; display: flex; gap: 6px; align-items: center; flex-wrap: wrap; justify-content: center; }

  @media (max-width: 760px) {
    .layout { grid-template-columns: 1fr; }
    aside { display: none; }
    .keys { display: none; }
  }
</style>
