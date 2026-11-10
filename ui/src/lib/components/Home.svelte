<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type CalendarStatus, type DaySummary, type PreviewCacheStatus, type ScanReport, type SourceInfo, type Stats, type Tag, type TagSuggestion } from '../api';
  import { minutesFor, rangeForBudget, shortRange } from '../days';
  import { formatBytes, formatCount } from '../format';
  import { viewport } from '../viewport.svelte';
  import Heatmap from './Heatmap.svelte';

  let { onStart }: { onStart: (range: { from: string; to: string }) => void } = $props();

  type Range = { from: string; to: string };

  let sources = $state<SourceInfo[]>([]);
  let stats = $state<Stats | null>(null);
  let days = $state<DaySummary[]>([]);
  let tags = $state<Tag[]>([]);
  let suggestions = $state<TagSuggestion[]>([]);
  let calendar = $state<CalendarStatus>('unavailable');
  let connecting = $state(false);
  let cache = $state<PreviewCacheStatus | null>(null);
  let confirmClear = $state(false);
  let clearing = $state(false);
  let cleared = $state<number | null>(null);
  let range = $state<Range | null>(null);
  let budget = $state(15);
  let order = $state<'oldest' | 'newest'>('oldest');
  let tagName = $state('');
  let report = $state<ScanReport | null>(null);
  let scanning = $state(false);
  let progress = $state({ done: 0, total: 0 });
  let error = $state<string | null>(null);

  const BUDGETS = [5, 15, 30, 60];

  const inRange = $derived(range ? days.filter((d) => d.day && d.day >= range!.from && d.day <= range!.to) : []);
  const left = $derived(inRange.reduce((n, d) => n + d.stats.undecided, 0));
  const total = $derived(inRange.reduce((n, d) => n + d.stats.total, 0));
  const tagsInRange = $derived(range ? tags.filter((t) => t.startDay <= range!.to && t.endDay >= range!.from) : []);
  const compact = $derived(viewport.compact);

  onMount(() => {
    refresh().then(() => applyBudget());
    const off = api.onEvent((e) => {
      if (e.type === 'scanProgress') progress = { done: e.done, total: e.total };
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
    [sources, stats, days, tags, suggestions, calendar] = await Promise.all([
      api.listSources(),
      api.stats(),
      api.days(),
      api.tags(),
      api.suggestTags(),
      api.calendarStatus(),
    ]);
  }

  async function connectCalendar() {
    connecting = true;
    try {
      calendar = await api.connectCalendar();
      suggestions = await api.suggestTags();
    } finally {
      connecting = false;
    }
  }

  function applyBudget() {
    const r = rangeForBudget(days, budget, order);
    if (r) range = r;
  }

  function pickBudget(m: number) {
    budget = m;
    applyBudget();
  }
  function pickOrder(o: 'oldest' | 'newest') {
    order = o;
    applyBudget();
  }

  function showTag(t: { startDay: string; endDay: string }) {
    range = { from: t.startDay, to: t.endDay };
  }

  async function acceptSuggestion(s: TagSuggestion) {
    await api.addTag(s.name, s.startDay, s.endDay, s.source);
    await refresh();
  }
  function dismissSuggestion(s: TagSuggestion) {
    suggestions = suggestions.filter((x) => x !== s);
  }
  async function addTag() {
    if (!range) return;
    await api.addTag(tagName.trim() || 'Untitled', range.from, range.to, 'manual');
    tagName = '';
    await refresh();
  }
  async function removeTag(id: number) {
    await api.removeTag(id);
    await refresh();
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
      applyBudget();
    }
  }

  async function remove(id: string) {
    await api.removeSource(id);
    await refresh();
  }

  function onKey(e: KeyboardEvent) {
    if (e.target instanceof HTMLInputElement) return;
    if (e.key === 'Enter' && range && left) onStart(range);
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="pick" class:compact>
  <aside>
    <div class="eyebrow">Sources</div>
    {#each sources as s (s.id)}
      <div class="src">
        <div>
          <strong>{s.name}</strong>
          <div class="muted small">{formatCount(s.photoCount)} photos</div>
        </div>
        <button class="ghost" title="Remove from this session (files are not touched)" onclick={() => remove(s.id)} disabled={scanning}>✕</button>
      </div>
    {/each}
    <button class="add" onclick={addFolder} disabled={scanning}>+ Add folder or SD card</button>
    {#if sources.length}<button class="ghost left" onclick={scan} disabled={scanning}>Rescan</button>{/if}

    <div class="eyebrow">Auto-tags</div>
    <ul class="how muted small">
      <li><span>⋯</span> Runs of shooting days</li>
      <li><span>▦</span> Calendar (when connected)</li>
      <li><span>◐</span> Soft photos flagged at scan</li>
    </ul>

    {#if stats}
      <div class="eyebrow">Library</div>
      <div class="kv"><span>Photos</span><b>{formatCount(stats.total)}</b></div>
      <div class="kv"><span>Left to cull</span><b>{formatCount(stats.undecided)}</b></div>
    {/if}

    {#if cache}
      {@const blocked = cache.connectedSources.length > 0}
      {@const size = formatBytes(cache.bytes)}
      <div class="eyebrow">Storage</div>
      <div class="kv"><span>App previews</span><b>{size}</b></div>

      {#if confirmClear && !blocked}
        <div class="confirm" role="alertdialog" aria-labelledby="clear-q">
          <b id="clear-q">Clear {size} of preview copies?</b>
          <p><span class="lbl">Removed</span>The screen-sized copies Camera Tinder made so you can browse quickly.</p>
          <p><span class="lbl">Not touched</span>Every photo, on cards, drives and this Mac. Your Keep / Nope decisions and tags.</p>
          <p class="muted">Previews are made again the next time you scan a card.</p>
          <div class="row">
            <button class="small primary" disabled={clearing} onclick={clearPreviews}>{clearing ? 'Clearing…' : `Clear ${size}`}</button>
            <button class="small ghost" disabled={clearing} onclick={() => (confirmClear = false)}>Cancel</button>
          </div>
        </div>
      {:else}
        <button class="small clear" disabled={blocked || cache.bytes === 0} onclick={clearPreviews}>Clear previews</button>
        <p class="muted small note">Frees space the app used for its own preview copies. Never touches your photos.</p>
        {#if blocked}
          <p class="small note wait">
            <b>Unplug first:</b> {cache.connectedSources.join(', ')}. Clearing waits until your cards and external drives are disconnected,
            so making previews again can't read from them.
          </p>
        {:else if cleared !== null}
          <p class="small note">Cleared {formatBytes(cleared)} of preview copies. Your photos, decisions and tags are unchanged.</p>
        {/if}
      {/if}
    {/if}
  </aside>

  <section class="main">
    <header>
      <div>
        <div class="eyebrow">Camera Tinder</div>
        <h1>What do you want to cull?</h1>
      </div>
      {#if compact}
        <div class="mbtns">
          <button onclick={addFolder} disabled={scanning}>+ Add</button>
          {#if sources.length}<button class="ghost" onclick={scan} disabled={scanning}>Rescan</button>{/if}
        </div>
      {/if}
    </header>

    {#if scanning}
      <div class="scan">
        <div class="muted">Scanning… {formatCount(progress.done)} / {formatCount(progress.total)}</div>
        <div class="progress"><div style:width="{progress.total ? (100 * progress.done) / progress.total : 0}%"></div></div>
      </div>
    {/if}

    {#if !days.length && !scanning}
      <p class="muted empty">
        Point Camera Tinder at a folder of photos or a mounted SD card, for example <code>/Volumes/NIKON Z 6</code>.
        The calendar fills in once photos are scanned.
      </p>
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

      {#if compact && tags.length}
        <div class="chips scroll">
          {#each tags as t (t.id)}
            <button class="chip" class:on={range?.from === t.startDay && range?.to === t.endDay} onclick={() => showTag(t)}>{t.name}</button>
          {/each}
        </div>
      {/if}

      <div class="group heat">
        <span class="eyebrow">Tags and calendar · drag to pick days</span>
        <Heatmap {days} {tags} {suggestions} {range} layout={compact ? 'months' : 'weeks'} onRange={(r) => (range = r)} onTag={showTag} />
        <div class="legend muted small">
          Photos still to cull: <i class="sw none"></i>none <i class="sw l1"></i><i class="sw l2"></i><i class="sw l3"></i><i class="sw l4"></i>many <i class="sw done"></i>all done
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
          <span class="eyebrow">Suggested tags · {suggestions.length}</span>
          {#each suggestions as s (s.name + s.startDay)}
            <div class="sug">
              <b>{s.name}</b>
              <span class="mono small">{shortRange(s.startDay, s.endDay)}</span>
              <span class="muted small why">{s.reason}</span>
              <span class="btns">
                <button class="small" onclick={() => showTag(s)}>Show</button>
                <button class="small primary" onclick={() => acceptSuggestion(s)}>Add</button>
                <button class="small ghost" onclick={() => dismissSuggestion(s)} aria-label="Dismiss">✕</button>
              </span>
            </div>
          {/each}
        </div>
      {/if}

      {#if tags.length && !compact}
        <div class="group">
          <span class="eyebrow">Your tags</span>
          <div class="chips">
            {#each tags as t (t.id)}
              <span class="chip tag">
                <button class="plain" onclick={() => showTag(t)}>{t.name}</button>
                <button class="plain x" onclick={() => removeTag(t.id)} aria-label="Remove tag {t.name}">✕</button>
              </span>
            {/each}
          </div>
        </div>
      {/if}
    {/if}
    {#if error}<p class="error">{error}</p>{/if}
    {#if report && !scanning && (report.skippedUnsupported || report.otherFiles)}
      <p class="leftbehind small">
        <b>Not handled by Camera Tinder, and not exported:</b>
        {#if report.skippedUnsupported}{formatCount(report.skippedUnsupported)} RAW{/if}{#if report.skippedUnsupported && report.otherFiles}{", "}{/if}{#if report.otherFiles}{formatCount(report.otherFiles)} other (videos etc.){/if}.
        Copy these yourself before you format the card.
      </p>
    {/if}
  </section>

  <div class="summary">
    {#if range}
      <div class="eyebrow">Selected</div>
      <div class="big">{formatCount(left)} <span class="muted">photos to cull</span></div>
      <div class="kv"><span>Range</span><b>{shortRange(range.from, range.to)}</b></div>
      {#if total - left > 0}<div class="kv"><span>Already culled</span><b>{formatCount(total - left)}</b></div>{/if}
      {#if tagsInRange.length}<div class="kv"><span>Tags</span><b>{tagsInRange.map((t) => t.name).join(', ')}</b></div>{/if}
      <div class="kv"><span>Time at your pace</span><b>~{minutesFor(left)} min</b></div>
      {#if !compact}
        <div class="tagform">
          <input placeholder="Name these days, e.g. Grandma's 80th" bind:value={tagName} onkeydown={(e) => e.key === 'Enter' && addTag()} />
          <button onclick={addTag}>Tag</button>
        </div>
      {/if}
      <button class="primary go" onclick={() => range && onStart(range)} disabled={!left || scanning}>Start culling{compact ? '' : ' ↵'}</button>
    {:else}
      <div class="muted">Pick days on the calendar to begin.</div>
    {/if}
  </div>
</div>

<style>
  .pick { display: grid; grid-template-columns: 220px minmax(0, 1fr) 280px; height: 100%; }
  aside { background: var(--panel); border-right: 1px solid var(--border); padding: 16px 14px; display: flex; flex-direction: column; gap: 8px; overflow-y: auto; }
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
  .empty { max-width: 56ch; }
  .scan { display: grid; gap: 8px; }
  code { font-size: 13px; }
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
  .sug { display: grid; grid-template-columns: minmax(0, 1.1fr) 120px minmax(0, 1.6fr) auto; gap: 10px; align-items: center; padding: 6px 0; border-top: 1px solid var(--border); }
  .why { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .btns { display: flex; gap: 6px; }
  button.small { padding: 3px 9px; font-size: 12px; border-radius: 6px; }
  .chip.tag { display: inline-flex; padding: 0; gap: 0; }
  .plain { background: transparent; border: 0; padding: 4px 6px 4px 11px; }
  .plain.x { padding: 4px 9px 4px 4px; color: var(--muted); }

  .summary { background: var(--panel); border-left: 1px solid var(--border); padding: 18px; display: flex; flex-direction: column; gap: 11px; }
  .big { font-size: 34px; font-weight: 700; letter-spacing: -0.02em; font-variant-numeric: tabular-nums; line-height: 1.1; }
  .big .muted { font-size: 14px; font-weight: 500; letter-spacing: 0; }
  .tagform { display: flex; gap: 6px; margin-top: 4px; }
  .tagform input { flex: 1; min-width: 0; font: inherit; padding: 7px 10px; border-radius: 8px; border: 1px solid var(--border); background: var(--bg); color: var(--text); }
  .go { margin-top: auto; padding: 12px; font-size: 16px; }

  /* Phones: one column; the summary becomes a bottom sheet. */
  .pick.compact { display: flex; flex-direction: column; }
  .pick.compact aside { display: none; }
  .pick.compact .main { flex: 1; min-height: 0; padding: 14px 16px 8px; gap: 10px; }
  .pick.compact .summary { border-left: 0; border-top: 1px solid var(--border); border-radius: 20px 20px 0 0; padding: 14px 16px calc(14px + env(safe-area-inset-bottom, 0px)); gap: 8px; }
  .pick.compact .go { margin-top: 4px; }
  .pick.compact .sug { grid-template-columns: 1fr auto; }
  .pick.compact .sug .why, .pick.compact .sug .mono { display: none; }
  .chips.scroll { flex-wrap: nowrap; overflow-x: auto; scrollbar-width: none; }
</style>
