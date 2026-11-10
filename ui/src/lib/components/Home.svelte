<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type CalendarStatus, type DaySummary, type PreviewCacheStatus, type ScanReport, type ScanRequest, type SourceInfo, type Stats, type TagSuggestion } from '../api';
  import { minutesFor, rangeForBudget, shortRange } from '../days';
  import { formatBytes, formatCount } from '../format';
  import { viewport } from '../viewport.svelte';
  import Heatmap from './Heatmap.svelte';
  import Splash from './Splash.svelte';
  import AppShell from './layout/AppShell.svelte';

  let { onStart }: { onStart: (days: string[]) => void } = $props();

  type Range = { from: string; to: string };

  let sources = $state<SourceInfo[]>([]);
  let stats = $state<Stats | null>(null);
  let days = $state<DaySummary[]>([]);
  let suggestions = $state<TagSuggestion[]>([]);
  let calendar = $state<CalendarStatus>('unavailable');
  let connecting = $state(false);
  let cache = $state<PreviewCacheStatus | null>(null);
  let confirmClear = $state(false);
  let advancedOpen = $state(false);
  let clearing = $state(false);
  let cleared = $state<number | null>(null);
  /** The one source of truth for what gets culled: selected days (`YYYY-MM-DD`). */
  let selected = $state(new Set<string>());
  /** Suggestion rows the user dismissed (by id); the rest is regrouped without them. */
  let dismissed = $state<string[]>([]);
  let budget = $state(15);
  let order = $state<'oldest' | 'newest'>('oldest');
  let report = $state<ScanReport | null>(null);
  let scanning = $state(false);
  let progress = $state({ done: 0, total: 0 });
  let stopping = $state(false);
  let error = $state<string | null>(null);

  const BUDGETS = [5, 15, 30, 60];

  const picked = $derived(days.filter((d) => d.day && selected.has(d.day)));
  const pickedDays = $derived(picked.map((d) => d.day as string).sort());
  /** First to last selected day, for labels only: culling gets exactly `pickedDays`. */
  const range = $derived<Range | null>(pickedDays.length ? { from: pickedDays[0], to: pickedDays[pickedDays.length - 1] } : null);
  const left = $derived(picked.reduce((n, d) => n + d.stats.undecided, 0));
  const total = $derived(picked.reduce((n, d) => n + d.stats.total, 0));
  const compact = $derived(viewport.compact);

  onMount(() => {
    refresh().then(() => applyBudget());
    const off = api.onEvent((e) => {
      if (e.type === 'scanProgress') progress = { done: e.done, total: e.total };
      // New photos were saved mid-scan: show them (days, counts, calendar suggestions) right away.
      if (e.type === 'scanSaved') liveRefresh();
    });
    // Cards come and go without telling us: re-check what's connected every few seconds.
    loadCache();
    const poll = setInterval(loadCache, 4000);
    return () => {
      clearInterval(poll);
      void off.then((f) => f());
    };
  });

  async function loadCache() {
    try {
      cache = await api.previewCacheStatus();
      if (cache.connectedSources.length) confirmClear = false;
    } catch {
      cache = null;
    }
  }

  async function clearPreviews() {
    if (!confirmClear) {
      confirmClear = true;
      return;
    }
    clearing = true;
    try {
      cleared = await api.clearPreviews();
      error = null;
    } catch (e) {
      error = String(e);
    } finally {
      clearing = false;
      confirmClear = false;
      await loadCache();
    }
  }

  async function refresh() {
    [sources, stats, days, suggestions, calendar] = await Promise.all([
      api.listSources(),
      api.stats(),
      api.days(),
      api.suggestTags(dismissed),
      api.calendarStatus(),
    ]);
  }

  async function connectCalendar() {
    connecting = true;
    try {
      calendar = await api.connectCalendar();
      suggestions = await api.suggestTags(dismissed);
    } finally {
      connecting = false;
    }
  }

  function applyBudget() {
    const r = rangeForBudget(days, budget, order);
    if (r) selected = new Set(daysWithPhotos(r.from, r.to));
  }

  function pickBudget(m: number) {
    budget = m;
    applyBudget();
  }
  function pickOrder(o: 'oldest' | 'newest') {
    order = o;
    applyBudget();
  }

  /** Days with photos between two days, inclusive. */
  function daysWithPhotos(from: string, to: string): string[] {
    return days.filter((d) => d.day && d.day >= from && d.day <= to && d.stats.total > 0).map((d) => d.day as string);
  }
  type Span = { startDay: string; endDay: string };
  /** How much of a suggestion's days are selected, read straight from the calendar. */
  function pickState(t: Span): 'none' | 'some' | 'all' {
    const ds = daysWithPhotos(t.startDay, t.endDay);
    const n = ds.filter((d) => selected.has(d)).length;
    return n === 0 ? 'none' : n === ds.length ? 'all' : 'some';
  }
  /** Adds a suggestion's days to the selection, or removes them when they're all selected already. */
  function togglePick(t: Span) {
    const all = pickState(t) === 'all';
    const next = new Set(selected);
    for (const d of daysWithPhotos(t.startDay, t.endDay)) {
      if (all) next.delete(d);
      else next.add(d);
    }
    selected = next;
  }
  const PICK_LABEL = { none: 'Select', some: 'Some', all: 'Selected' } as const;

  /** Hides the row everywhere and regroups the rest (its children may move under another row).
   *  The selection is untouched. */
  async function dismissSuggestion(s: TagSuggestion) {
    dismissed = [...dismissed, s.id];
    suggestions = await api.suggestTags(dismissed);
  }
  let expanded = $state(new Set<string>());
  function toggle(s: TagSuggestion) {
    const k = s.id;
    const next = new Set(expanded);
    if (!next.delete(k)) next.add(k);
    expanded = next;
  }

  // At most one refresh in flight, and at most one every LIVE_REFRESH_MS, while a scan saves photos.
  const LIVE_REFRESH_MS = 1200;
  let liveTimer: ReturnType<typeof setTimeout> | null = null;
  let lastLive = 0;
  function liveRefresh() {
    if (liveTimer) return;
    liveTimer = setTimeout(async () => {
      lastLive = Date.now();
      try {
        await refresh();
        if (!selected.size) applyBudget();
      } finally {
        liveTimer = null;
      }
    }, Math.max(0, lastLive + LIVE_REFRESH_MS - Date.now()));
  }

  async function addFolder() {
    error = null;
    try {
      const path = await api.pickSourceFolder();
      if (!path) return;
      const added = await api.addFolder(path);
      sources = await api.listSources();
      await scan({ sourceIds: [added.id] }); // only the new folder; the others haven't changed
    } catch (e) {
      error = String(e);
    }
  }

  /** Every folder: lists each one and reads only files it hasn't indexed yet. */
  const rescanAll = () => scan();

  async function scan(request?: ScanRequest) {
    error = null;
    scanning = true;
    stopping = false;
    progress = { done: 0, total: 0 };
    try {
      report = await api.scan(request);
    } catch (e) {
      error = String(e);
    } finally {
      scanning = false;
      stopping = false;
      await refresh();
      applyBudget();
    }
  }

  async function stopScan() {
    stopping = true;
    await api.cancelScan();
  }

  async function remove(id: string) {
    await api.removeSource(id);
    await refresh();
  }

  function onKey(e: KeyboardEvent) {
    if (e.target instanceof HTMLInputElement) return;
    if (e.key === 'Enter' && left) onStart(pickedDays);
    if (e.key === 'Escape' && selected.size) selected = new Set();
  }
</script>

<svelte:window onkeydown={onKey} />

{#snippet sidebar()}
  <aside>
    <div class="eyebrow">Sources</div>
    <button class="add" onclick={addFolder} disabled={scanning}>+ Add photos</button>
    {#each sources as s (s.id)}
      <div class="src">
        <div>
          <strong>{s.name}</strong>
          <div class="muted small">{formatCount(s.photoCount)} photos</div>
        </div>
        <span class="src-btns">
          <button class="ghost" title="Remove from this session (files are not touched)" onclick={() => remove(s.id)} disabled={scanning}>✕</button>
        </span>
      </div>
    {/each}
    {#if sources.length}<button class="ghost left" title="Scan every folder for new photos (photos already scanned aren't read again)" onclick={rescanAll} disabled={scanning}>Rescan</button>{/if}

    <div class="eyebrow">Suggestions from</div>
    <ul class="how muted small">
      <li><span>⋯</span> Runs of shooting days</li>
      <li><span>▦</span> Calendar (when connected)</li>
    </ul>

    {#if stats}
      <div class="eyebrow">Library</div>
      <div class="kv"><span>Photos</span><b>{formatCount(stats.total)}</b></div>
      <div class="kv"><span>Left to triage</span><b>{formatCount(stats.undecided)}</b></div>
    {/if}

    {#if cache}
      {@const blocked = cache.connectedSources.length > 0}
      {@const size = formatBytes(cache.bytes)}
      <!-- Pinned to the bottom of the rail; opens upward. -->
      <div class="advanced" class:open={advancedOpen}>
        {#if advancedOpen}
          <div class="adv-panel" id="adv-panel">
            <div class="kv"><span>App previews</span><b>{size}</b></div>

            {#if confirmClear && !blocked}
              <div class="confirm" role="alertdialog" aria-labelledby="clear-q">
                <b id="clear-q">Clear {size} of preview copies?</b>
                <p><span class="lbl">Removed</span>The screen-sized copies Camera Tinder made so you can browse quickly.</p>
                <p><span class="lbl">Not touched</span>Every photo, on cards, drives and this Mac. Your Keep / Nope decisions.</p>
                <p class="muted">Previews are made again the next time you scan a card.</p>
                <div class="row">
                  <button class="small primary" disabled={clearing || scanning} onclick={clearPreviews}>{clearing ? 'Clearing…' : `Clear ${size}`}</button>
                  <button class="small ghost" disabled={clearing} onclick={() => (confirmClear = false)}>Cancel</button>
                </div>
              </div>
            {:else}
              <button class="small clear" disabled={blocked || scanning || cache.bytes === 0} title={scanning ? "Wait for the scan to finish" : undefined} onclick={clearPreviews}>Clear previews</button>
              <p class="muted small note">Frees space the app used for its own preview copies. Never touches your photos.</p>
              {#if blocked}
                <p class="small note wait">
                  <b>Unplug first:</b> {cache.connectedSources.join(', ')}. Clearing waits until your cards and external drives are disconnected,
                  so making previews again can't read from them.
                </p>
              {:else if cleared !== null}
                <p class="small note">Cleared {formatBytes(cleared)} of preview copies. Your photos and decisions are unchanged.</p>
              {/if}
            {/if}
          </div>
        {/if}
        <button class="plain adv-toggle" aria-expanded={advancedOpen} aria-controls="adv-panel" onclick={() => (advancedOpen = !advancedOpen)}>
          <span>{advancedOpen ? '▾' : '▴'} Advanced</span>
          <span class="mono muted">Cache: {size}</span>
        </button>
      </div>
    {/if}
  </aside>
{/snippet}

<AppShell left={sidebar} right={summary}>
  <section class="main">
    {#if days.length || scanning}
    <header>
      <div>
        <div class="eyebrow">Camera Tinder</div>
        <h1>What do you want to triage?</h1>
      </div>
      {#if compact}
        <div class="mbtns">
          <button onclick={addFolder} disabled={scanning}>+ Add</button>
          {#if sources.length}<button class="ghost" onclick={rescanAll} disabled={scanning}>Rescan</button>{/if}
        </div>
      {/if}
    </header>
    {/if}

    {#if scanning}
      <div class="scan">
        <div class="scan-h">
          <span class="muted grow">{stopping ? 'Stopping after the photos being read…' : `Scanning… ${formatCount(progress.done)} / ${formatCount(progress.total)}`}</span>
          <button class="small" onclick={stopScan} disabled={stopping}>Stop</button>
        </div>
        <div class="progress"><div style:width="{progress.total ? (100 * progress.done) / progress.total : 0}%"></div></div>
      </div>
    {/if}

    {#if !days.length && !scanning}
      <Splash onAdd={addFolder} busy={scanning} />
    {:else}
      <div class="group">
        <span class="eyebrow">Time you have</span>
        <div class="chips">
          {#each BUDGETS as m (m)}
            <button class="chip" class:on={budget === m} onclick={() => pickBudget(m)}>{m} min</button>
          {/each}
          <button class="chip" class:on={order === 'oldest'} onclick={() => pickOrder('oldest')}>Oldest first</button>
          <button class="chip" class:on={order === 'newest'} onclick={() => pickOrder('newest')}>Newest first</button>
        </div>
      </div>

      <div class="group heat">
        <span class="eyebrow">Calendar · click or drag to pick days</span>
        <Heatmap {days} {selected} onChange={(next) => (selected = next)} />
        <div class="legend muted small">
          Photos still to triage: <i class="sw none"></i>none <i class="sw l1"></i><i class="sw l2"></i><i class="sw l3"></i><i class="sw l4"></i>many <i class="sw done"></i>all done
        </div>
      </div>

      {#if calendar === 'notDetermined' || calendar === 'denied'}
        <div class="group calprompt">
          <span class="eyebrow">Calendar</span>
          {#if calendar === 'notDetermined'}
            <span class="muted small">Name your shoots from Apple Calendar events. Stays on this device.</span>
            <button class="small primary" disabled={connecting} onclick={connectCalendar}>{connecting ? 'Waiting for macOS…' : 'Connect Calendar'}</button>
          {:else}
            <span class="muted small">Calendar access is off. Turn it on for Camera Tinder in System Settings → Privacy &amp; Security → Calendars.</span>
          {/if}
        </div>
      {/if}

      {#if suggestions.length}
        <div class="group">
          <span class="eyebrow">Suggestions · {suggestions.length}</span>
          {#snippet sugRow(s: TagSuggestion, child: boolean)}
            {@const open = expanded.has(s.id)}
            {@const pick = pickState(s)}
            <div class="sug" class:child>
              <span class="nm">
                {#if !s.children.length && !child && suggestions.some((x) => x.children.length)}
                  <span class="chev" aria-hidden="true"></span>
                {:else if s.children.length}
                  <button class="plain chev" onclick={() => toggle(s)} aria-expanded={open} aria-label={open ? 'Collapse' : 'Expand'}>{open ? '▾' : '▸'}</button>
                {/if}
                {#if s.names.length > 1}
                  <span class="names">{#each s.names as n (n)}<b title={n}>{n}</b>{/each}</span>
                {:else}
                  <b title={s.name}>{s.name}</b>
                {/if}
                {#if s.children.length}<span class="muted small count">{s.children.length}</span>{/if}
              </span>
              <span class="mono small">{shortRange(s.startDay, s.endDay)}</span>
              <span class="muted small why">{s.reason}</span>
              <span class="btns">
                <button class="small pick {pick}" aria-pressed={pick === 'all'} onclick={() => togglePick(s)}>{pick === 'all' ? '✓ ' : ''}{PICK_LABEL[pick]}</button>
                <button class="small ghost" onclick={() => dismissSuggestion(s)} aria-label="Dismiss">✕</button>
              </span>
            </div>
          {/snippet}
          {#each suggestions as s (s.id)}
            {@render sugRow(s, false)}
            {#if expanded.has(s.id)}
              {#each s.children as c (c.id)}
                {@render sugRow(c, true)}
              {/each}
            {/if}
          {/each}
        </div>
      {/if}

    {/if}
    {#if error}<p class="error">{error}</p>{/if}
    {#if report && !scanning && report.cancelled}
      <p class="muted small">Scan stopped. {formatCount(report.new)} new photos kept; Rescan to pick up the rest.</p>
    {:else if report && !scanning && !report.new && report.found && !report.failed.length}
      <p class="muted small">No new photos since the last scan.</p>
    {/if}
    {#if report && !scanning && (report.skippedUnsupported || report.otherFiles)}
      <p class="leftbehind small">
        <b>Not handled by Camera Tinder, and not exported:</b>
        {#if report.skippedUnsupported}{formatCount(report.skippedUnsupported)} RAW{/if}{#if report.skippedUnsupported && report.otherFiles}{", "}{/if}{#if report.otherFiles}{formatCount(report.otherFiles)} other (videos etc.){/if}.
        Copy these yourself before you format the card.
      </p>
    {/if}
  </section>
</AppShell>

{#snippet summary()}
  <div class="summary">
    {#if range}
      <div class="sel-h">
        <span class="eyebrow">Selected</span>
        <button class="small ghost" onclick={() => (selected = new Set())}>Clear</button>
      </div>
      <div class="big">{formatCount(left)} <span class="muted">photos to triage</span></div>
      <div class="kv"><span>{pickedDays.length === 1 ? '1 day' : `${pickedDays.length} days`}</span><b>{shortRange(range.from, range.to)}</b></div>
      {#if total - left > 0}<div class="kv"><span>Already triaged</span><b>{formatCount(total - left)}</b></div>{/if}
      <div class="kv"><span>Time at your pace</span><b>~{minutesFor(left)} min</b></div>
      <button class="primary go" onclick={() => onStart(pickedDays)} disabled={!left || scanning}>Start triage{compact ? '' : ' ↵'}</button>
    {:else}
      <div class="muted">Pick days on the calendar to begin.</div>
    {/if}
  </div>
{/snippet}

<style>
  aside { display: flex; flex-direction: column; gap: 8px; }
  .advanced { margin-top: auto; padding-top: 12px; display: flex; flex-direction: column; gap: 8px; }
  .adv-panel { display: flex; flex-direction: column; gap: 8px; padding-bottom: 4px; }
  .adv-toggle { display: flex; justify-content: space-between; align-items: center; gap: 8px; width: 100%; padding: 8px 0 0; border-top: 1px solid var(--border); border-radius: 0; font-size: 12.5px; color: var(--muted); text-align: left; }
  .adv-toggle:hover { color: var(--text); background: transparent; }
  .advanced.open .adv-toggle { color: var(--text); }
  aside .eyebrow { margin-top: 12px; }
  aside .eyebrow:first-child { margin-top: 0; }
  .src { display: flex; justify-content: space-between; align-items: center; gap: 8px; padding: 6px 8px; border-radius: 8px; background: var(--panel-2); }
  .add { text-align: left; }
  .left { justify-self: start; text-align: left; color: var(--muted); }
  .how { list-style: none; margin: 0; padding: 0; display: grid; gap: 4px; }
  .how span { font-family: ui-monospace, 'SF Mono', monospace; margin-right: 6px; }
  .small { font-size: 12.5px; }
  .kv { display: flex; justify-content: space-between; gap: 8px; color: var(--muted); font-size: 12.5px; border-top: 1px solid var(--border); padding-top: 7px; }
  .kv b { color: var(--text); font-weight: 500; font-family: ui-monospace, 'SF Mono', monospace; text-align: right; }

  .main { padding: 22px 24px; display: flex; flex-direction: column; gap: 16px; overflow-y: auto; min-width: 0; }
  header { display: flex; justify-content: space-between; align-items: flex-start; gap: 12px; }
  .mbtns { display: flex; gap: 6px; flex: none; }
  h1 { font-size: 24px; margin: 2px 0 0; letter-spacing: -0.01em; }
  .group { display: grid; gap: 8px; }
  .scan { display: grid; gap: 8px; }
  .scan-h { display: flex; align-items: center; gap: 10px; }
  .scan-h .grow { flex: 1; min-width: 0; }
  .src-btns { display: flex; gap: 2px; flex: none; }
  .legend { display: flex; gap: 6px; align-items: center; flex-wrap: wrap; }
  .sw { width: 14px; height: 14px; border-radius: 3px; display: inline-block; background: color-mix(in srgb, var(--accent) var(--c, 0%), var(--cell0)); }
  .sw.l1 { --c: 16%; } .sw.l2 { --c: 36%; } .sw.l3 { --c: 62%; } .sw.l4 { --c: 94%; }
  .sw.none { background: transparent; box-shadow: inset 0 0 0 1px var(--border); }
  .sw.done { background: transparent; box-shadow: inset 0 0 0 1.5px var(--done); }

  .leftbehind { border: 1px solid var(--border); border-left: 3px solid var(--accent); border-radius: 6px; padding: 8px 10px; }
  .clear { text-align: left; }
  .note { margin: 0; line-height: 1.35; }
  .wait { border-left: 3px solid var(--accent); padding-left: 8px; }
  .confirm { display: grid; gap: 6px; padding: 10px; border: 1px solid var(--border); border-radius: 8px; background: var(--panel-2); font-size: 12.5px; }
  .confirm p { margin: 0; line-height: 1.35; }
  .confirm .lbl { display: block; font: 500 10.5px ui-monospace, 'SF Mono', monospace; text-transform: uppercase; letter-spacing: 0.04em; color: var(--muted); }
  .confirm .row { display: flex; gap: 6px; }
  .calprompt { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; }
  .sug { display: grid; grid-template-columns: minmax(0, 1.7fr) 120px minmax(0, 1fr) auto; gap: 10px; align-items: center; padding: 6px 0; border-top: 1px solid var(--border); }
  .nm { display: flex; align-items: baseline; gap: 6px; min-width: 0; }
  .nm b { min-width: 0; }
  .names { display: grid; gap: 3px; min-width: 0; padding: 3px 0; }
  .names b { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .chev { align-self: center; }
  button.pick { min-width: 78px; }
  button.pick.some { border-style: dashed; }
  button.pick.all { background: var(--text); color: var(--bg); border-color: var(--text); }
  .sel-h { display: flex; justify-content: space-between; align-items: center; }
  .chev { width: 1.4em; flex: none; color: var(--muted); font-size: 1.1em; text-align: left; }
  .count { flex: none; }
  .sug.child { padding-left: 2.8em; }
  .sug.child .nm b { font-weight: 500; }
  .why { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .btns { display: flex; gap: 6px; }
  button.small { padding: 3px 9px; font-size: 12px; border-radius: 6px; }
  .plain { background: transparent; border: 0; padding: 4px 6px 4px 11px; }

  .summary { display: flex; flex-direction: column; gap: 11px; }
  .big { font-size: 34px; font-weight: 700; letter-spacing: -0.02em; font-variant-numeric: tabular-nums; line-height: 1.1; }
  .big .muted { font-size: 14px; font-weight: 500; letter-spacing: 0; }
  .go { margin-top: auto; padding: 12px; font-size: 16px; }

  /* Phones: AppShell hides the sidebar and turns the summary into a bottom sheet. */
  :global(.shell.compact) .main { padding: 14px 16px 8px; gap: 10px; }
  :global(.shell.compact) .summary { gap: 8px; }
  :global(.shell.compact) .go { margin-top: 4px; }
  :global(.shell.compact) .sug { grid-template-columns: 1fr auto; }
  :global(.shell.compact) .sug .why, :global(.shell.compact) .sug .mono { display: none; }
</style>
