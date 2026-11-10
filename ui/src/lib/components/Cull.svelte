<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type CoreEvent, type Decision, type Group, type Photo, type Place, type TriageItem } from '../api';
  import { DECISIONS, ORDER, decisionForKey } from '../decisions';
  import { daysLabel, minutesFor } from '../days';
  import { formatCount, formatDay, formatTime, previewEdge } from '../format';
  import { viewport } from '../viewport.svelte';
  import {
    ISSUE_LABELS,
    firstUndecidedFrame,
    leaveNext,
    loopNext,
    loopPrev,
    groupOf,
    nextStop,
    nextUndecidedFrame,
    sameOrder,
    setAsideLeft,
    stepFrame,
    stopOf,
    structure,
  } from '../triage';
  import BurstStrip from './BurstStrip.svelte';
  import Filmstrip from './Filmstrip.svelte';
  import MomentsBar from './MomentsBar.svelte';
  import SwipeCard from './SwipeCard.svelte';
  import AppShell from './layout/AppShell.svelte';

  let {
    days,
    focusId = null,
    onExit,
    onReview,
    onRescue,
  }: {
    days: string[];
    focusId?: number | null;
    onExit: () => void;
    onReview: () => void;
    onRescue: () => void;
  } = $props();

  interface Ghost { key: number; photo: Photo; decision: Decision; x: number; y: number }
  type Sel = { from: number; to: number };

  const edge = previewEdge();
  const PRELOAD = 4;

  let photos = $state<Photo[]>([]);
  let moments = $state<Group[]>([]);
  let triage = $state<TriageItem[]>([]);
  let triageVersion = '';
  /** A set-aside photo the user opened from the filmstrip; shown instead of the cursor's photo. */
  let opened = $state<number | null>(null);
  let idx = $state(0);
  let selection = $state<Sel | null>(null);
  let timeline = $state<'moments' | 'strip'>('moments');
  let ghosts = $state<Ghost[]>([]);
  let flash = $state<{ key: number; d: Decision } | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let coached = $state(false);
  let ghostKey = 0;

  const compact = $derived(viewport.compact);
  const s = $derived(structure(triage));
  /** The photo on screen: the opened set-aside photo, else the cursor (a single, or a frame of a group). */
  const shown = $derived(opened ?? idx);
  const current = $derived<Photo | null>(photos[shown] ?? null);
  const item = $derived<TriageItem | null>(triage[shown] ?? null);
  /** The frames of the group the cursor is on (null for a single, or while a set-aside photo is open). */
  const members = $derived(opened === null && item?.group ? groupOf(s, idx) : null);
  const setAsideCount = $derived(setAsideLeft(s, photos).length);

  // Per-photo lookups built from the moment and burst groups.
  const momentOf = $derived.by(() => {
    const out: number[] = new Array(photos.length).fill(0);
    moments.forEach((g, m) => { for (let i = g.start; i < g.end; i++) out[i] = m; });
    return out;
  });

  const decided = $derived(photos.filter((p) => p.decision).length);
  const left = $derived(photos.length - decided);
  const momentNo = $derived(Math.min((momentOf[idx] ?? moments.length) + 1, moments.length));
  const leftInMoment = $derived.by(() => {
    const g = moments[momentOf[idx]];
    if (!g) return 0;
    let n = 0;
    for (let i = g.start; i < g.end; i++) if (!photos[i].decision && !s.setAside[i]) n++; // Shift+arrow skips set-aside photos
    return n;
  });

  // Decisions run strictly in order so the backend undo stack stays correct.
  let chain: Promise<unknown> = Promise.resolve();
  function enqueue<T>(f: () => Promise<T>): Promise<T> {
    const p = chain.then(f);
    chain = p.catch((e) => (error = String(e)));
    return p;
  }

  onMount(() => {
    const off = api.onEvent(onCoreEvent);
    load();
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
      clearTimeout(refreshTimer);
      void off.then((f) => f());
    };
  });

  // Analysis keeps running in the background. If some photos weren't analysed when this screen opened,
  // pick up new results (bursts, set-aside photos) every few seconds as they arrive, without moving the cursor.
  let refreshTimer: ReturnType<typeof setTimeout> | undefined;
  function onCoreEvent(e: CoreEvent) {
    if (e.type !== 'analysisProgress' || refreshTimer || !triage.some((t) => !t.analyzed)) return;
    refreshTimer = setTimeout(async () => {
      refreshTimer = undefined;
      try {
        const plan = await api.plan({ days, status: 'all' });
        if (sameOrder(photos, plan.triage)) triage = plan.triage; // same photos only: a scan may have changed the list
      } catch {
        // The next progress event tries again.
      }
    }, 3000);
  }

  async function load() {
    loading = true;
    try {
      const plan = await api.plan({ days, status: 'all' });
      photos = plan.photos;
      moments = plan.moments;
      triage = plan.triage;
      triageVersion = plan.triageVersion;
      const at = focusId != null ? photos.findIndex((p) => p.id === focusId) : -1;
      if (at >= 0 && s.setAside[at]) opened = at;
      idx = at >= 0 && !s.setAside[at] ? at : land(nextStop(s, photos, -1));
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  /** Where the cursor goes for a stop returned by `nextStop`: its first undecided frame, or the end (-1). */
  function land(stop: number): number {
    return stop < 0 ? photos.length : firstUndecidedFrame(s, photos, stop);
  }

  /** Move to photo `i` without opening set-aside photos (they snap to the next stop). Moments bar, days list. */
  function snap(i: number) {
    selection = null;
    opened = null;
    idx = s.setAside[i] ? land(nextStop(s, photos, i)) : firstUndecidedFrame(s, photos, i);
  }

  const preloaded = new Map<number, HTMLImageElement>();
  $effect(() => {
    const upcoming: Photo[] = [];
    for (let from = idx; upcoming.length < PRELOAD; ) {
      const n = nextStop(s, photos, from);
      if (n < 0 || n === idx || upcoming.some((p) => p.id === photos[n].id)) break;
      upcoming.push(photos[n]);
      from = n;
    }
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

  /** Apply one decision to many photos (by index), locally and in the backend, as one undo step. */
  function apply(indices: number[], d: Decision, place: Place = 'timeline') {
    const list = indices.filter((i) => photos[i]);
    if (!list.length) return;
    for (const i of list) photos[i].decision = d;
    const ids = list.map((i) => photos[i].id);
    enqueue(() => (ids.length === 1 ? api.decide(ids[0], d) : api.decideMany(ids, d)));
    // Log what triage suggested next to what was decided, to tune the suggestions later.
    const outcomes = list.filter((i) => triage[i]).map((i) => ({ item: triage[i], planVersion: triageVersion, place, decision: d }));
    // A failed log must not look like a failed decision: the decision is saved either way.
    if (outcomes.length) enqueue(() => api.logOutcomes(outcomes).catch((e) => console.warn('outcome log failed', e)));
    flash = { key: ghostKey++, d };
    coached = true;
  }

  /** After deciding photo `at`: the next undecided frame of its group, else the next stop. */
  function advanceFrom(at: number): number {
    return nextUndecidedFrame(s, photos, at) ?? land(nextStop(s, photos, stopOf(s, at)));
  }

  function decide(d: Decision, from?: { x: number; y: number }) {
    if (selection) {
      const sel = selection;
      selection = null;
      // Set-aside photos are left for Rescue; a range decision doesn't touch them.
      const all = Array.from({ length: sel.to - sel.from + 1 }, (_, k) => sel.from + k).filter((i) => !s.setAside[i]);
      apply(all, d, 'bulk');
      idx = land(nextStop(s, photos, sel.to));
      return;
    }
    const p = current;
    if (!p) return;
    if (from && (from.x || from.y)) ghosts.push({ key: ghostKey++, photo: { ...p }, decision: d, x: from.x, y: from.y });
    const at = shown;
    apply([at], d, opened !== null ? 'openedEarly' : 'timeline');
    if (opened !== null) opened = null; // back to where the cursor was
    else idx = advanceFrom(at);
  }

  function decideMoment(d: Decision) {
    const g = moments[momentOf[idx]];
    if (!g) return;
    const list: number[] = [];
    for (let i = g.start; i < g.end; i++) if (!photos[i].decision && !s.setAside[i]) list.push(i);
    apply(list, d, 'bulk');
    opened = null;
    idx = land(nextStop(s, photos, g.end - 1));
  }

  async function undo() {
    const back = await enqueue(() => api.undo());
    if (!back.length) return;
    let first = photos.length;
    for (const b of back) {
      const i = photos.findIndex((p) => p.id === b.id);
      if (i < 0) continue;
      photos[i].decision = b.decision;
      first = Math.min(first, i);
    }
    selection = null;
    if (first < photos.length) {
      // A set-aside photo can't hold the cursor: show it opened instead.
      opened = s.setAside[first] ? first : null;
      if (opened === null) idx = first;
    }
  }

  /** Next (Space): loops inside the current burst, or over the undecided photos; never leaves one unrated. */
  function skip() {
    if (!current || opened !== null) return;
    const n = loopNext(s, photos, idx);
    if (n !== null) idx = n;
  }

  /** Back (B): one photo back without changing anything; the mirror of Space. */
  function back() {
    if (!current || opened !== null) return;
    const n = loopPrev(s, idx);
    if (n !== null) idx = n;
  }

  /** Leave (Enter / S): out of the burst, or off this photo, to the next undecided one, leaving it unrated. */
  function leave() {
    if (!current || opened !== null) return;
    const n = leaveNext(s, photos, idx);
    if (n !== null) idx = n;
  }

  /** A click on the filmstrip: set-aside photos open in place; anything else moves the cursor there. */
  function jump(i: number) {
    selection = null;
    if (i >= photos.length) {
      opened = null;
      idx = photos.length;
    } else if (s.setAside[Math.max(0, i)]) {
      opened = Math.max(0, i);
    } else {
      opened = null;
      idx = firstUndecidedFrame(s, photos, Math.max(0, i));
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.target instanceof HTMLInputElement || e.metaKey && e.key !== 'z') return;
    const d = decisionForKey(e.key);
    if (d) {
      e.preventDefault();
      if (e.repeat) return;
      e.shiftKey ? decideMoment(d) : decide(d);
    } else if (e.key === 'Backspace' || (e.key === 'z' && (e.metaKey || e.ctrlKey)) || e.key === 'u') {
      e.preventDefault();
      undo();
    } else if (e.key === ' ') {
      e.preventDefault();
      skip();
    } else if (e.key === 'b') {
      e.preventDefault();
      back();
    } else if (e.key === 'Enter' || e.key === 's') {
      e.preventDefault();
      leave();
    } else if (e.key === '[' || e.key === ']') {
      const frame = opened === null ? stepFrame(s, idx, e.key === ']' ? 1 : -1, true) : null;
      if (frame !== null) idx = frame;
    } else if (e.key === 'Escape') {
      if (opened !== null) opened = null;
      else if (selection) selection = null;
      else onExit();
    }
  }

  const rangeLabel = $derived(daysLabel(days));

  // Days list in the inspector: progress per day, click to jump.
  const dayRows = $derived.by(() => {
    const map = new Map<string, { day: string; start: number; n: number; done: number }>();
    photos.forEach((p, i) => {
      const day = p.capturedAt?.slice(0, 10) ?? 'unknown';
      const r = map.get(day) ?? { day, start: i, n: 0, done: 0 };
      r.n++;
      if (p.decision) r.done++;
      map.set(day, r);
    });
    return [...map.values()];
  });

  /** First undecided photo among `n` photos starting at `start`, else the start. */
  function nextUndecidedIn(start: number, n: number): number {
    for (let i = start; i < start + n; i++) if (!photos[i].decision) return i;
    return start;
  }

  const nextPhoto = $derived.by(() => {
    const n = nextStop(s, photos, shown);
    return n >= 0 && n !== idx ? photos[n] : null;
  });
  const groupPos = $derived(item?.group ? groupOf(s, idx).indexOf(idx) + 1 : 0);
</script>

{#snippet topBar()}
  <header class="top">
    <button onclick={onExit}>{compact ? 'Done' : '‹ Pick'}</button>
    <div class="title">
      <b>{rangeLabel}</b>
      <span class="mono muted">moment {momentNo}/{moments.length} · {formatCount(decided)}/{formatCount(photos.length)} · ~{minutesFor(left)} min</span>
    </div>
    {#if !compact}
      <div class="prog"><div style:width="{photos.length ? (100 * decided) / photos.length : 0}%"></div></div>
      <span class="grow"></span>
      <span class="seg" role="group" aria-label="Timeline style">
        <button class:on={timeline === 'moments'} onclick={() => (timeline = 'moments')}>Moments</button>
        <button class:on={timeline === 'strip'} onclick={() => (timeline = 'strip')}>Strip</button>
      </span>
    {/if}
    {#if setAsideCount}<button onclick={onRescue}>Rescue {setAsideCount}</button>{/if}
    <button onclick={onReview}>Review</button>
  </header>
{/snippet}

<AppShell top={topBar} right={compact ? undefined : inspector} bottom={bottomBar} overflow="visible">
    <div class="stage-col">
      {#if opened !== null && item}
        <div class="banner">
          <span><b>Set aside · {item.reasons.map((r) => ISSUE_LABELS[r]).join(', ') || 'looks bad'}.</b> You opened it early. Decide now, or leave it for Rescue.</span>
          <button class="small" onclick={() => (opened = null)}>Leave for Rescue{compact ? '' : ' (esc)'}</button>
        </div>
      {:else if members && members.every((m) => triage[m].reasons.length)}
        <div class="banner pick"><span><b>Every frame has issues.</b> It stays here because you shot a burst.</span></div>
      {/if}
      <div class="stage">
        {#if current}
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
          {#if compact && nextPhoto}<img class="peek photo-fill" src={api.previewUrl(nextPhoto.id, edge)} alt="" />{/if}
          {#key current.id}
            <SwipeCard photo={current} {edge} onDecide={decide} />
          {/key}
          <div class="chips-over mono">
            <span>{formatTime(current.capturedAt)}</span>
            {#if item?.group && members}<span>{item.group.kind === 'burst' ? 'burst' : 'similar'} {groupPos}/{item.group.size}</span>{/if}
            {#if item?.group?.suggested && members}<span>★ suggested</span>{/if}
            {#if item}
              {#each item.reasons as r (r)}<span class="bad">{ISSUE_LABELS[r]}</span>{/each}
              {#each item.hints as h (h)}<span class="hint">{ISSUE_LABELS[h]}</span>{/each}
            {/if}
            {#if current.decision}<span style:color="var(--{current.decision})">{DECISIONS[current.decision].icon} {DECISIONS[current.decision].label}</span>{/if}
          </div>
          {#if compact && !coached}
            <div class="coach">Swipe the photo<br /><small>← Nope · Keep → · ↑ Favorite · ↓ Hate</small></div>
          {/if}
        {:else if !loading}
          <div class="done">
            <div class="big">{formatCount(photos.length)}</div>
            photos decided in {moments.length} moments.
            {#if setAsideCount}
              <span>{setAsideCount} looked bad and were set aside.</span>
              <button class="primary" onclick={onRescue}>Open Rescue</button>
              <button onclick={onReview}>Review &amp; export</button>
            {:else}
              <button class="primary" onclick={onReview}>Review &amp; export</button>
            {/if}
          </div>
        {/if}
        {#if flash}
          {#key flash.key}<div class="flash {flash.d}">{DECISIONS[flash.d].icon}</div>{/key}
        {/if}
        {#if error}<p class="error toast">{error}</p>{/if}
      </div>

      {#if members && current}
        <BurstStrip {photos} {triage} {members} frame={idx} {compact} onPick={(i) => (idx = i)} />
      {/if}
      {#if !compact && current}
        <div class="under muted">
          <span>{formatDay(current.capturedAt?.slice(0, 10) ?? null)} · {formatTime(current.capturedAt)}</span>
          <span><kbd>⇧</kbd> + arrow: {leftInMoment} left in this moment</span>
        </div>
      {/if}
    </div>
</AppShell>

{#snippet inspector()}
      <aside class="insp">
        <div class="eyebrow">Photo</div>
        {#if current}
          <div class="kv first"><span>File</span><b>{current.fileName}</b></div>
          <div class="kv"><span>Taken</span><b>{formatDay(current.capturedAt?.slice(0, 10) ?? null)} {formatTime(current.capturedAt)}</b></div>
          {#if current.camera}<div class="kv"><span>Camera</span><b>{current.camera}</b></div>{/if}
          <div class="kv"><span>Burst</span><b>{item?.group ? `${groupPos} of ${item.group.size}` : '—'}</b></div>
        {/if}
        <div class="eyebrow days-h">Days</div>
        <div class="days">
          {#each dayRows.slice(0, 14) as d (d.day)}
            <button class="dr" class:now={current?.capturedAt?.startsWith(d.day)} onclick={() => snap(nextUndecidedIn(d.start, d.n))}>
              <span>{d.day === 'unknown' ? '?' : formatDay(d.day).replace(/^\w+, /, '')}</span>
              <i><b style:width="{(100 * d.done) / d.n}%"></b></i>
              <span class="muted r">{d.done}/{d.n}</span>
            </button>
          {/each}
        </div>
      </aside>
{/snippet}

{#snippet bottomBar()}
  <div class="bottom">
    {#if selection}
      <div class="selbar">
        <b>{selection.to - selection.from + 1} selected</b>
        <span class="muted grow">{compact ? '' : 'Press a decision key to apply to all'}</span>
        {#each ORDER as d (d)}
          <button class="small" style:color="var(--{d})" onclick={() => decide(d)}>{DECISIONS[d].icon}{compact ? '' : ' ' + DECISIONS[d].label}</button>
        {/each}
        <button class="small" onclick={() => (selection = null)}>{compact ? 'Cancel' : 'Esc'}</button>
      </div>
    {/if}
    {#if timeline === 'moments' || compact}
      <MomentsBar {photos} {moments} {idx} {selection} setAside={s.setAside} {compact} onSelect={(from, to) => (selection = { from, to })} onJump={snap} />
    {/if}
    <Filmstrip
      {photos}
      {triage}
      {s}
      idx={shown}
      {selection}
      {momentOf}
      back={compact ? 3 : timeline === 'strip' ? 9 : 6}
      forward={compact ? 3 : timeline === 'strip' ? 9 : 6}
      size={compact ? 36 : timeline === 'strip' ? 58 : 50}
      onSelect={(from, to) => (selection = { from, to })}
      onJump={jump}
    />
    {#if compact}
      <div class="actions">
        <button class="round sm" onclick={undo} aria-label="Undo">↶</button>
        {#each ORDER as d (d)}
          <button class="round {d}" onclick={() => decide(d)} disabled={!current} aria-label={DECISIONS[d].label}>{DECISIONS[d].icon}</button>
        {/each}
        <button class="round sm" onclick={skip} disabled={!current} aria-label="Next">»</button>
      </div>
    {:else}
      <div class="keys muted">
        <span><kbd>←</kbd> ✕ nope</span><span><kbd>→</kbd> ✓ keep</span><span><kbd>↑</kbd> ★ favorite</span><span><kbd>↓</kbd> ⊘ hate</span>
        <span><kbd>space</kbd> next</span><span><kbd>b</kbd> back</span><span><kbd>enter</kbd> / <kbd>s</kbd> {members ? 'leave burst' : 'skip'}</span>{#if members}<span><kbd>[</kbd><kbd>]</kbd> frame</span>{/if}<span><kbd>⌫</kbd> undo</span><span><kbd>⇧</kbd>+arrow whole moment</span><span>drag to select</span>
      </div>
    {/if}
  </div>
{/snippet}

<style>
  .top { display: flex; align-items: center; gap: 12px; padding: 0 4px; flex-wrap: nowrap; }
  .top > button { flex: none; }
  .prog { width: 160px; height: 6px; border-radius: 3px; background: var(--panel-2); overflow: hidden; flex: none; }
  .prog > div { height: 100%; background: var(--accent); }
  .grow { flex: 1; min-width: 0; }
  .seg { display: inline-flex; border: 1px solid var(--border); border-radius: 8px; overflow: hidden; }
  .seg button { border: 0; border-radius: 0; background: var(--panel); padding: 4px 10px; font-size: 12px; }
  .seg button + button { border-left: 1px solid var(--border); }
  .seg .on { background: var(--text); color: var(--bg); }

  .stage-col { display: flex; flex-direction: column; gap: 10px; padding: 12px 16px 8px; min-height: 0; min-width: 0; }
  .stage { position: relative; flex: 1; min-height: 0; }
  .under { display: flex; justify-content: center; gap: 14px; font-size: 12px; min-height: 18px; }
  button.small { padding: 3px 9px; font-size: 12px; border-radius: 6px; }

  .chips-over { position: absolute; left: 12px; bottom: 10px; display: flex; gap: 6px; font-size: 11px; color: #fff; z-index: 3; pointer-events: none; flex-wrap: wrap; }
  .chips-over span { background: #000a; border-radius: 5px; padding: 2px 7px; }
  .chips-over span.hint { background: color-mix(in srgb, var(--favorite) 80%, #000); color: #1a1200; }
  .chips-over span.bad { background: var(--hate); }
  .banner { display: flex; gap: 10px; align-items: center; justify-content: space-between; flex-wrap: wrap; font-size: 13px; padding: 8px 12px; border-radius: 10px; border: 1px solid color-mix(in srgb, var(--hate) 50%, var(--border)); background: color-mix(in srgb, var(--hate) 9%, var(--panel)); }
  .banner.pick { border-color: color-mix(in srgb, var(--pick) 50%, var(--border)); background: color-mix(in srgb, var(--pick) 9%, var(--panel)); }

  .ghost-card { position: absolute; inset: 0; pointer-events: none; animation: 0.32s ease-in forwards; z-index: 2; }
  .fly-like { animation-name: fly-right; }
  .fly-dislike { animation-name: fly-left; }
  .fly-favorite { animation-name: fly-up; }
  .fly-hate { animation-name: fly-down; }
  @keyframes fly-right { from { transform: translate(var(--x0), var(--y0)) rotate(var(--r0)); } to { transform: translate(110vw, var(--y0)) rotate(24deg); opacity: 0.4; } }
  @keyframes fly-left { from { transform: translate(var(--x0), var(--y0)) rotate(var(--r0)); } to { transform: translate(-110vw, var(--y0)) rotate(-24deg); opacity: 0.4; } }
  @keyframes fly-up { from { transform: translate(var(--x0), var(--y0)) rotate(var(--r0)); } to { transform: translate(var(--x0), -110vh) scale(0.9); opacity: 0.4; } }
  @keyframes fly-down { from { transform: translate(var(--x0), var(--y0)) rotate(var(--r0)); } to { transform: translate(var(--x0), 110vh) scale(0.9); opacity: 0.4; } }
  .peek { transform: scale(0.93) translateY(20px); opacity: 0.5; filter: saturate(0.5); }
  .flash { position: absolute; left: 50%; top: 50%; translate: -50% -50%; width: 92px; height: 92px; border-radius: 50%; display: grid; place-items: center; font-size: 44px; color: #fff; background: var(--c); animation: flash 0.5s ease-out forwards; pointer-events: none; z-index: 6; }
  .flash.like { --c: var(--like); } .flash.dislike { --c: var(--dislike); } .flash.favorite { --c: var(--favorite); } .flash.hate { --c: var(--hate); }
  @keyframes flash { from { opacity: 0.95; scale: 0.7; } to { opacity: 0; scale: 1.35; } }
  .coach { position: absolute; left: 50%; bottom: 36px; translate: -50% 0; background: #000d; color: #fff; border-radius: 12px; padding: 7px 12px; font-size: 12px; text-align: center; pointer-events: none; z-index: 4; }
  .coach small { opacity: 0.75; font-size: 11px; }
  .done { position: absolute; inset: 0; display: grid; place-content: center; justify-items: center; gap: 12px; text-align: center; color: var(--muted); }
  .done .big { font-size: 40px; font-weight: 700; color: var(--text); }
  .toast { position: absolute; bottom: 8px; left: 50%; translate: -50% 0; background: var(--panel); padding: 6px 12px; border-radius: 8px; z-index: 7; }

  .insp { display: flex; flex-direction: column; gap: 6px; overflow: hidden; }
  .kv { display: flex; justify-content: space-between; gap: 8px; color: var(--muted); font-size: 12.5px; border-top: 1px solid var(--border); padding-top: 7px; }
  .kv.first { border: 0; padding: 0; }
  .kv b { color: var(--text); font-weight: 500; font-family: ui-monospace, 'SF Mono', monospace; text-align: right; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .days-h { margin-top: 10px; }
  .days { display: flex; flex-direction: column; gap: 2px; overflow: hidden; }
  .dr { display: grid; grid-template-columns: 56px 1fr 48px; gap: 8px; align-items: center; padding: 4px 6px; border: 0; border-radius: 6px; background: transparent; font: 500 11.5px ui-monospace, 'SF Mono', monospace; text-align: left; }
  .dr:hover { background: var(--panel-2); }
  .dr.now { background: var(--panel-2); box-shadow: inset 2px 0 var(--accent); }
  .dr i { height: 5px; border-radius: 3px; background: var(--panel-2); overflow: hidden; display: block; }
  .dr i b { display: block; height: 100%; background: var(--done); }
  .r { text-align: right; }

  .bottom { padding: 0 4px; display: flex; flex-direction: column; gap: 9px; }
  .selbar { display: flex; align-items: center; gap: 8px; padding: 6px 8px 6px 12px; border-radius: 10px; background: color-mix(in srgb, var(--accent) 12%, var(--panel)); border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--border)); font-size: 12.5px; flex-wrap: wrap; }
  .keys { display: flex; flex-wrap: wrap; gap: 4px 14px; font-size: 11.5px; justify-content: center; }

  /* Phone layout: swipe-first card stack, small action row. */
  :global(.shell.compact) .top { padding: max(8px, env(safe-area-inset-top, 0px)) 14px 8px; }
  :global(.shell.compact) .stage-col { padding: 8px 20px 0; }
  :global(.shell.compact) .bottom { padding: 8px 16px calc(10px + env(safe-area-inset-bottom, 0px)); }
  .actions { display: flex; gap: 8px; justify-content: center; align-items: center; }
  .round { width: 52px; height: 52px; border-radius: 50%; padding: 0; font-size: 22px; color: var(--c, var(--muted)); border: 1.5px solid var(--c, var(--border)); background: color-mix(in srgb, var(--c, var(--muted)) 10%, var(--panel)); }
  .round.sm { width: 38px; height: 38px; font-size: 16px; --c: var(--muted); }
  .round.like { --c: var(--like); } .round.dislike { --c: var(--dislike); } .round.favorite { --c: var(--favorite); } .round.hate { --c: var(--hate); }
</style>
