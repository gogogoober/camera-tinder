<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type Decision, type Photo, type StatusFilter } from '../api';
  import { DECISIONS, ORDER } from '../decisions';
  import { daysLabel } from '../days';
  import { formatCount, formatDay } from '../format';
  import { dragSelect } from '../select';
  import { viewport } from '../viewport.svelte';
  import Badge from './Badge.svelte';
  import AppShell from './layout/AppShell.svelte';

  let {
    days: pickedDays,
    onBack,
    onOpen,
    onExport,
  }: {
    days: string[] | null;
    onBack: () => void;
    onOpen: (photoId: number) => void;
    /** Opens Export for what's on screen: the decisions of the current filter and the day (or picked days). */
    onExport: (decisions: Decision[], days: string[] | null) => void;
  } = $props();

  type Filter = 'all' | 'favorite' | 'like' | 'dislike' | 'hate' | 'todo';
  const FILTERS: [Filter, string][] = [
    ['all', 'All'],
    ['favorite', '★ Favorite'],
    ['like', '✓ Keep'],
    ['dislike', '✕ Nope'],
    ['hate', '⊘ Hate'],
    ['todo', 'Not yet'],
  ];
  const PAGE = 300;

  let photos = $state<Photo[]>([]);
  let filter = $state<Filter>('all');
  let day = $state<string | null>(null);
  let selection = $state<{ from: number; to: number } | null>(null);
  let shown = $state(PAGE);
  let error = $state<string | null>(null);
  const compact = $derived(viewport.compact);

  onMount(load);

  async function load() {
    try {
      const status: StatusFilter = 'all';
      photos = await api.queue({ days: pickedDays, status });
    } catch (e) {
      error = String(e);
    }
  }

  const counts = $derived.by(() => {
    const c: Record<Filter, number> = { all: photos.length, favorite: 0, like: 0, dislike: 0, hate: 0, todo: 0 };
    for (const p of photos) {
      if (p.decision) c[p.decision]++;
      else c.todo++;
    }
    return c;
  });

  const dayRows = $derived.by(() => {
    const m = new Map<string, Record<string, number>>();
    for (const p of photos) {
      const d = p.capturedAt?.slice(0, 10) ?? 'unknown';
      const r = m.get(d) ?? { n: 0 };
      r.n++;
      if (p.decision) r[p.decision] = (r[p.decision] ?? 0) + 1;
      m.set(d, r);
    }
    return [...m.entries()];
  });

  const list = $derived(
    photos.filter((p) => {
      if (day && !(p.capturedAt ?? 'unknown').startsWith(day)) return false;
      if (filter === 'all') return true;
      if (filter === 'todo') return !p.decision;
      return p.decision === filter;
    }),
  );
  const visible = $derived(list.slice(0, shown));
  /** What "Export these" copies: the decisions of the filter on screen (All = the keepers). */
  const exportDecisions = $derived<Decision[]>(filter === 'all' ? ['favorite', 'like'] : filter === 'todo' ? [] : [filter]);
  const exportDays = $derived(day ? [day] : pickedDays);
  /** What the Export screen will start with: these decisions and days, minus photos exported before. */
  let exportCount = $state(0);
  /** The same, exported before included: the button stays usable to export them again. */
  let exportAll = $state(0);
  /** Bumped once a decision change is saved, so the count is asked for after the write. */
  let saved = $state(0);
  let exportTicket = 0;
  $effect(() => {
    const ds = exportDecisions;
    const scope = exportDays;
    void saved; // re-count after a decision change is saved
    const mine = ++exportTicket;
    if (!ds.length) {
      exportCount = exportAll = 0;
      return;
    }
    const opts = {
      includeFavorites: ds.includes('favorite'),
      includeLikes: ds.includes('like'),
      includeDislikes: ds.includes('dislike'),
      includeHates: ds.includes('hate'),
      days: scope,
    };
    Promise.all([api.exportCount({ ...opts, includeExported: false }), api.exportCount({ ...opts, includeExported: true })])
      .then(([fresh, all]) => {
        if (mine !== exportTicket) return;
        exportCount = fresh;
        exportAll = all;
      })
      .catch(() => {});
  });

  function setFilter(f: Filter) {
    filter = f;
    selection = null;
    shown = PAGE;
  }
  function setDay(d: string | null) {
    day = d;
    selection = null;
    shown = PAGE;
  }

  async function redecide(d: Decision) {
    if (!selection) return;
    const picked = visible.slice(selection.from, selection.to + 1);
    selection = null;
    for (const p of picked) p.decision = d;
    try {
      await api.decideMany(picked.map((p) => p.id), d);
      saved++;
    } catch (e) {
      error = String(e);
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      if (selection) selection = null;
      else onBack();
    } else if (e.key === 'z' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      api.undo().then((back) => {
        for (const b of back) {
          const p = photos.find((x) => x.id === b.id);
          if (p) p.decision = b.decision;
        }
        saved++;
      });
    }
  }
</script>

<svelte:window onkeydown={onKey} />

{#snippet topBar()}
  <header class="top">
    <button onclick={onBack}>{compact ? 'Triage' : '‹ Back to triage'}</button>
    <b>Review{pickedDays ? ` · ${daysLabel(pickedDays)}` : ''}</b>
    <span class="grow"></span>
    {#if !compact}<span class="muted">Drag to select · click to reopen</span>{/if}
  </header>
{/snippet}

{#snippet days()}
      <aside>
        <div class="eyebrow">Days</div>
        <button class="d" class:on={day === null} onclick={() => setDay(null)}><b>All days</b><small class="muted">{formatCount(photos.length)} photos</small></button>
        {#each dayRows.slice(0, 14) as [d, r] (d)}
          <button class="d" class:on={day === d} onclick={() => setDay(d)}>
            <b>{d === 'unknown' ? 'Unknown date' : formatDay(d)} <span class="mono muted">{r.n}</span></b>
            <span class="mix">
              {#each ['favorite', 'like', 'dislike', 'hate'] as k (k)}<i style:width="{((r[k] ?? 0) / r.n) * 100}%" style:background="var(--{k})"></i>{/each}
            </span>
          </button>
        {/each}
      </aside>
{/snippet}

<AppShell top={topBar} left={days}>
    <section class="main">
      <div class="bar">
        <div class="chips" class:scroll={compact}>
          {#each FILTERS as [k, label] (k)}
            <button class="chip" class:on={filter === k} onclick={() => setFilter(k)}>{label} <span class="mono">{counts[k]}</span></button>
          {/each}
        </div>
        {#if selection}
          <div class="selbar">
            <b>{selection.to - selection.from + 1} selected</b>
            <span class="grow muted">{compact ? '' : 'Give them all one decision'}</span>
            {#each ORDER as d (d)}
              <button class="small" style:color="var(--{d})" onclick={() => redecide(d)}>{DECISIONS[d].icon}{compact ? '' : ' ' + DECISIONS[d].label}</button>
            {/each}
            <button class="small" onclick={() => (selection = null)}>{compact ? 'Cancel' : 'Esc'}</button>
          </div>
        {/if}
      </div>

      <div
        class="grid"
        use:dragSelect={{ onSelect: (from, to) => (selection = { from, to }), onClick: (i) => onOpen(visible[i].id) }}
      >
        {#each visible as p, i (p.id)}
          <span class="gi" class:todo={!p.decision} class:selected={!!selection && i >= selection.from && i <= selection.to} data-i={i} title={p.fileName}>
            <img src={api.previewUrl(p.id, 256)} alt="" draggable="false" loading="lazy" />
            <Badge decision={p.decision} />
          </span>
        {/each}
        {#if !visible.length}<p class="muted empty">Nothing here.</p>{/if}
      </div>
      {#if list.length > shown}<button class="more" onclick={() => (shown += PAGE)}>Show {formatCount(Math.min(PAGE, list.length - shown))} more</button>{/if}
      {#if error}<p class="error">{error}</p>{/if}

      <footer>
        <div><b>{formatCount(counts.like + counts.favorite)} keepers</b> <span class="muted">· ✓ {counts.like} + ★ {counts.favorite}{compact ? '' : ' · originals never touched'}</span></div>
        <button class="primary" onclick={() => onExport(exportDecisions, exportDays)} disabled={!exportAll} title={exportCount ? "Export the originals of what this filter shows that weren't exported before; adjust on the next screen" : 'Everything here was exported before; the next screen exports it again'}>
          {compact ? 'Share' : 'Export'}{exportCount ? '' : ' again'} {formatCount(exportCount || exportAll)}{filter === 'all' || filter === 'todo' ? '' : ` ${FILTERS.find((f) => f[0] === filter)?.[1]}`}
        </button>
      </footer>
    </section>
</AppShell>

<style>
  .top { display: flex; align-items: center; gap: 12px; padding: 0 4px; }
  .grow { flex: 1; min-width: 0; }
  aside { display: flex; flex-direction: column; gap: 6px; }
  .d { display: grid; gap: 5px; text-align: left; background: transparent; border-color: transparent; padding: 7px 10px; }
  .d.on { background: var(--panel-2); border-color: var(--border); }
  .mix { display: flex; height: 6px; border-radius: 3px; overflow: hidden; background: var(--panel-2); }
  .mix i { display: block; }
  .main { min-width: 0; display: flex; flex-direction: column; min-height: 0; }
  .bar { padding: 10px 16px; border-bottom: 1px solid var(--border); display: grid; gap: 8px; }
  .chips.scroll { flex-wrap: nowrap; overflow-x: auto; scrollbar-width: none; }
  .selbar { display: flex; align-items: center; gap: 8px; padding: 6px 8px 6px 12px; border-radius: 10px; background: color-mix(in srgb, var(--accent) 12%, var(--panel)); border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--border)); font-size: 12.5px; flex-wrap: wrap; }
  button.small { padding: 3px 9px; font-size: 12px; border-radius: 6px; }
  .grid { flex: 1; min-height: 0; overflow-y: auto; display: grid; grid-template-columns: repeat(auto-fill, minmax(92px, 1fr)); grid-auto-rows: min-content; gap: 8px; padding: 14px 16px; align-content: start; user-select: none; }
  .gi { position: relative; aspect-ratio: 1; border-radius: 8px; cursor: pointer; }
  .gi img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; border-radius: 8px; pointer-events: none; }
  .gi.todo img { filter: grayscale(0.9) brightness(0.7); }
  .gi.selected { outline: 2px solid var(--accent); outline-offset: 2px; }
  .empty { grid-column: 1 / -1; }
  .more { margin: 0 16px 8px; align-self: center; }
  footer { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 12px 16px calc(12px + env(safe-area-inset-bottom, 0px)); border-top: 1px solid var(--border); }
  footer button { white-space: nowrap; flex: none; }
  :global(.shell.compact) .top { padding: max(8px, env(safe-area-inset-top, 0px)) 14px 8px; border-bottom: 1px solid var(--border); }
  :global(.shell.compact) .grid { grid-template-columns: repeat(3, 1fr); gap: 3px; padding: 6px 0 0; }
  :global(.shell.compact) .gi, :global(.shell.compact) .gi img { border-radius: 0; }
</style>
