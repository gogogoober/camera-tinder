<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type Decision, type Group, type Photo } from '../api';
  import { DECISIONS, ORDER, decisionForKey } from '../decisions';
  import { minutesFor, shortRange } from '../days';
  import { formatCount, formatDay, formatTime, isSoft, previewEdge } from '../format';
  import { viewport } from '../viewport.svelte';
  import Filmstrip from './Filmstrip.svelte';
  import MomentsBar from './MomentsBar.svelte';
  import SwipeCard from './SwipeCard.svelte';

  let {
    range,
    focusId = null,
    onExit,
    onReview,
  }: {
    range: { from: string; to: string };
    focusId?: number | null;
    onExit: () => void;
    onReview: () => void;
  } = $props();

  interface Ghost { key: number; photo: Photo; decision: Decision; x: number; y: number }
  type Sel = { from: number; to: number };

  const edge = previewEdge();
  const PRELOAD = 4;

  let photos = $state<Photo[]>([]);
  let moments = $state<Group[]>([]);
  let bursts = $state<Group[]>([]);
  let idx = $state(0);
  let selection = $state<Sel | null>(null);
  let mode = $state<'cull' | 'triage' | 'compare'>('cull');
  let triageSet = $state(new Set<number>());
  let softLater = $state(false);
  let timeline = $state<'moments' | 'strip'>('moments');
  let ghosts = $state<Ghost[]>([]);
  let flash = $state<{ key: number; d: Decision } | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let coached = $state(false);
  let ghostKey = 0;

  const compact = $derived(viewport.compact);
  const current = $derived<Photo | null>(photos[idx] ?? null);

  // Per-photo lookups built from the moment and burst groups.
  const momentOf = $derived.by(() => {
    const out: number[] = new Array(photos.length).fill(0);
    moments.forEach((g, m) => { for (let i = g.start; i < g.end; i++) out[i] = m; });
    return out;
  });
  const burstOf = $derived.by(() => {
    const out: number[] = new Array(photos.length).fill(-1);
    bursts.forEach((g, b) => { for (let i = g.start; i < g.end; i++) out[i] = b; });
    return out;
  });
  const burstSizeAt = $derived(new Map(bursts.map((g) => [g.start, g.end - g.start])));
  const burst = $derived(burstOf[idx] >= 0 ? bursts[burstOf[idx]] : null);

  const decided = $derived(photos.filter((p) => p.decision).length);
  const left = $derived(photos.length - decided);
  const momentNo = $derived(Math.min((momentOf[idx] ?? moments.length) + 1, moments.length));
  const softTodo = $derived(photos.map((p, i) => i).filter((i) => !photos[i].decision && isSoft(photos[i])));
  const leftInMoment = $derived.by(() => {
    const g = moments[momentOf[idx]];
    return g ? photos.slice(g.start, g.end).filter((p) => !p.decision).length : 0;
  });

  // Decisions run strictly in order so the backend undo stack stays correct.
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

  async function load() {
    loading = true;
    try {
      const plan = await api.plan({ day: range.from, dayTo: range.to, status: 'all' });
      photos = plan.photos;
      moments = plan.moments;
      bursts = plan.bursts;
      const at = focusId != null ? photos.findIndex((p) => p.id === focusId) : -1;
      idx = at >= 0 ? at : nextUndecided(0);
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  /** First undecided photo at or after `from`, wrapping round; photos.length when none. */
  function nextUndecided(from: number): number {
    for (let i = from; i < photos.length; i++) if (!photos[i].decision) return i;
    for (let i = 0; i < Math.min(from, photos.length); i++) if (!photos[i].decision) return i;
    return photos.length;
  }

  const preloaded = new Map<number, HTMLImageElement>();
  $effect(() => {
    const upcoming: Photo[] = [];
    for (let i = idx + 1; i < photos.length && upcoming.length < PRELOAD; i++) if (!photos[i].decision) upcoming.push(photos[i]);
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
  function apply(indices: number[], d: Decision) {
    const list = indices.filter((i) => photos[i]);
    if (!list.length) return;
    for (const i of list) photos[i].decision = d;
    const ids = list.map((i) => photos[i].id);
    enqueue(() => (ids.length === 1 ? api.decide(ids[0], d) : api.decideMany(ids, d)));
    flash = { key: ghostKey++, d };
    coached = true;
  }

  function decide(d: Decision, from?: { x: number; y: number }) {
    if (selection) {
      const sel = selection;
      selection = null;
      const all = Array.from({ length: sel.to - sel.from + 1 }, (_, k) => sel.from + k);
      apply(all, d);
      idx = nextUndecided(sel.to + 1);
      return;
    }
    const p = current;
    if (!p) return;
    if (from && (from.x || from.y)) ghosts.push({ key: ghostKey++, photo: { ...p }, decision: d, x: from.x, y: from.y });
    const at = idx;
    apply([at], d);
    idx = nextUndecided(at + 1);
  }

  function decideMoment(d: Decision) {
    const g = moments[momentOf[idx]];
    if (!g) return;
    const list: number[] = [];
    for (let i = g.start; i < g.end; i++) if (!photos[i].decision) list.push(i);
    apply(list, d);
    idx = nextUndecided(g.end);
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
    mode = 'cull';
    if (first < photos.length) idx = first;
  }

  function skip() {
    if (!current) return;
    idx = nextUndecided(idx + 1);
  }

  function jump(i: number) {
    selection = null;
    if (mode === 'compare') mode = 'cull';
    idx = Math.max(0, Math.min(photos.length, i));
  }

  function openTriage() {
    triageSet = new Set(softTodo);
    selection = null;
    mode = 'triage';
  }
  function confirmTriage() {
    const list = [...triageSet];
    mode = 'cull';
    apply(list, 'dislike');
    if (current?.decision) idx = nextUndecided(idx);
  }
  function toggleTriage(i: number) {
    const next = new Set(triageSet);
    if (next.has(i)) next.delete(i);
    else next.add(i);
    triageSet = next;
  }

  /** Burst frames and the sharpest one (marked as the suggested keeper). */
  const frames = $derived.by(() => {
    if (!burst) return [] as number[];
    return Array.from({ length: burst.end - burst.start }, (_, k) => burst.start + k);
  });
  const sharpest = $derived(frames.reduce((best, i) => ((photos[i].sharpness ?? -1) > (photos[best]?.sharpness ?? -1) ? i : best), frames[0] ?? 0));

  function keepFrame(keep: number) {
    const others = frames.filter((i) => i !== keep);
    apply(others, 'dislike');
    apply([keep], 'like');
    mode = 'cull';
    idx = nextUndecided(burst ? burst.end : idx + 1);
  }

  function onKey(e: KeyboardEvent) {
    if (e.target instanceof HTMLInputElement || e.metaKey && e.key !== 'z') return;
    if (mode === 'triage') {
      if (e.key === 'Enter') { e.preventDefault(); confirmTriage(); }
      else if (e.key === 'Escape') mode = 'cull';
      return;
    }
    if (mode === 'compare') {
      const n = parseInt(e.key, 10);
      if (n >= 1 && n <= frames.length) { e.preventDefault(); keepFrame(frames[n - 1]); }
      else if (e.key === 'Escape' || e.key === 'b') mode = 'cull';
      return;
    }
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
    } else if (e.key === 'b' && burst) {
      mode = 'compare';
    } else if (e.key === 'Escape') {
      if (selection) selection = null;
      else onExit();
    }
  }

  const rangeLabel = $derived(shortRange(range.from, range.to));

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
    for (let i = idx + 1; i < photos.length; i++) if (!photos[i].decision) return photos[i];
    return null;
  });
</script>

<div class="cull" class:compact>
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
    <button onclick={onReview}>Review</button>
  </header>

  <div class="body">
    <div class="stage-col">
      {#if softTodo.length && !softLater && mode === 'cull'}
        <div class="banner">
          <b>◐ {softTodo.length} photos look soft</b>
          <span class="muted grow">Low sharpness score. Clear them as one batch first?</span>
          <button class="primary small" onclick={openTriage}>Review {softTodo.length}</button>
          <button class="small" onclick={() => (softLater = true)}>Later</button>
        </div>
      {/if}

      <div class="stage">
        {#if mode === 'triage'}
          <div class="pane">
            <div class="pane-h">
              <b>Soft photos</b>
              <span class="muted grow">{triageSet.size} of {softTodo.length} will be marked Nope. {compact ? 'Tap' : 'Click'} to keep one out.</span>
              <button class="primary small" onclick={confirmTriage}>Nope {triageSet.size}</button>
              <button class="small" onclick={() => (mode = 'cull')}>Cancel</button>
            </div>
            <div class="tgrid">
              {#each softTodo as i (i)}
                <button class="ti" class:off={!triageSet.has(i)} onclick={() => toggleTriage(i)} aria-pressed={triageSet.has(i)}>
                  <img src={api.previewUrl(photos[i].id, 256)} alt="" draggable="false" />
                  <span class="score">{photos[i].sharpness}</span>
                  {#if triageSet.has(i)}<em class="x">✕</em>{/if}
                </button>
              {/each}
            </div>
          </div>
        {:else if mode === 'compare' && burst}
          <div class="pane">
            <div class="pane-h">
              <b>Burst of {frames.length}</b>
              <span class="muted grow">{compact ? 'Tap the keeper.' : `Press 1–${frames.length} or click the keeper.`} The rest become Nope.</span>
              <button class="small" onclick={() => (mode = 'cull')}>{compact ? 'Close' : 'Esc'}</button>
            </div>
            <div class="cmp" style:--n={compact ? 2 : Math.min(frames.length, 5)}>
              {#each frames as i, k (i)}
                <button class="cf" class:best={i === sharpest} onclick={() => keepFrame(i)}>
                  <img src={api.previewUrl(photos[i].id, 1024)} alt="Frame {k + 1}" draggable="false" />
                  {#if !compact}<span class="num mono">{k + 1}</span>{/if}
                  <span class="lbl mono">
                    {i === sharpest ? '✓ sharpest · ' : ''}sharpness {photos[i].sharpness ?? '–'}
                    <i class="meter"><b class:bad={isSoft(photos[i])} style:width="{photos[i].sharpness ?? 0}%"></b></i>
                  </span>
                </button>
              {/each}
            </div>
          </div>
        {:else if current}
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
            {#if burst}<span>burst {idx - burst.start + 1}/{burst.end - burst.start}</span>{/if}
            {#if isSoft(current)}<span class="warn">◐ soft · {current.sharpness}</span>{/if}
            {#if current.decision}<span style:color="var(--{current.decision})">{DECISIONS[current.decision].icon} {DECISIONS[current.decision].label}</span>{/if}
          </div>
          {#if compact && !coached}
            <div class="coach">Swipe the photo<br /><small>← Nope · Keep → · ↑ Favorite · ↓ Hate</small></div>
          {/if}
        {:else if !loading}
          <div class="done">
            <div class="big">{formatCount(photos.length)}</div>
            photos decided in {moments.length} moments.
            <button class="primary" onclick={onReview}>Review &amp; export</button>
          </div>
        {/if}
        {#if flash}
          {#key flash.key}<div class="flash {flash.d}">{DECISIONS[flash.d].icon}</div>{/key}
        {/if}
        {#if error}<p class="error toast">{error}</p>{/if}
      </div>

      {#if !compact && mode === 'cull' && current}
        <div class="under muted">
          <span>{formatDay(current.capturedAt?.slice(0, 10) ?? null)} · {formatTime(current.capturedAt)}</span>
          {#if burst}<span><kbd>B</kbd> compare burst of {burst.end - burst.start}</span>{/if}
          <span><kbd>⇧</kbd> + arrow: {leftInMoment} left in this moment</span>
        </div>
      {/if}
    </div>

    {#if !compact}
      <aside class="insp">
        <div class="eyebrow">Photo</div>
        {#if current}
          <div class="kv first"><span>File</span><b>{current.fileName}</b></div>
          <div class="kv"><span>Taken</span><b>{formatDay(current.capturedAt?.slice(0, 10) ?? null)} {formatTime(current.capturedAt)}</b></div>
          {#if current.camera}<div class="kv"><span>Camera</span><b>{current.camera}</b></div>{/if}
          <div class="kv col">
            <span class="between">Sharpness <b style:color={isSoft(current) ? 'var(--soft)' : 'var(--text)'}>{current.sharpness ?? '–'}{current.sharpness != null ? (isSoft(current) ? ' · soft' : ' · sharp') : ''}</b></span>
            <i class="meter"><b class:bad={isSoft(current)} style:width="{current.sharpness ?? 0}%"></b></i>
          </div>
          <div class="kv"><span>Burst</span><b>{burst ? `${idx - burst.start + 1} of ${burst.end - burst.start}` : '—'}</b></div>
        {/if}
        <div class="eyebrow days-h">Days</div>
        <div class="days">
          {#each dayRows.slice(0, 14) as d (d.day)}
            <button class="dr" class:now={current?.capturedAt?.startsWith(d.day)} onclick={() => jump(nextUndecidedIn(d.start, d.n))}>
              <span>{d.day === 'unknown' ? '?' : formatDay(d.day).replace(/^\w+, /, '')}</span>
              <i><b style:width="{(100 * d.done) / d.n}%"></b></i>
              <span class="muted r">{d.done}/{d.n}</span>
            </button>
          {/each}
        </div>
      </aside>
    {/if}
  </div>

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
      <MomentsBar {photos} {moments} {idx} {selection} {compact} onSelect={(from, to) => (selection = { from, to })} onJump={jump} />
    {/if}
    <Filmstrip
      {photos}
      {idx}
      {selection}
      {momentOf}
      {burstSizeAt}
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
        <button class="round sm" onclick={skip} disabled={!current} aria-label="Skip">»</button>
      </div>
      {#if burst && mode === 'cull'}<button class="small burst" onclick={() => (mode = 'compare')}>Compare burst ×{burst.end - burst.start}</button>{/if}
    {:else}
      <div class="keys muted">
        <span><kbd>←</kbd> ✕ nope</span><span><kbd>→</kbd> ✓ keep</span><span><kbd>↑</kbd> ★ favorite</span><span><kbd>↓</kbd> ⊘ hate</span>
        <span><kbd>space</kbd> skip</span><span><kbd>⌫</kbd> undo</span><span><kbd>⇧</kbd>+arrow whole moment</span><span><kbd>B</kbd> compare burst</span><span>drag to select</span>
      </div>
    {/if}
  </div>
</div>

<style>
  .cull { display: grid; grid-template-rows: auto 1fr auto; grid-template-columns: minmax(0, 1fr); height: 100%; min-height: 0; overflow: hidden; }
  .top { display: flex; align-items: center; gap: 12px; padding: 8px 14px; flex-wrap: nowrap; }
  .title { display: flex; flex-direction: column; line-height: 1.25; min-width: 0; flex: 1; }
  .title span { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .top > button { flex: none; }
  .title span { font-size: 12px; }
  .prog { width: 160px; height: 6px; border-radius: 3px; background: var(--panel-2); overflow: hidden; flex: none; }
  .prog > div { height: 100%; background: var(--accent); }
  .grow { flex: 1; min-width: 0; }
  .seg { display: inline-flex; border: 1px solid var(--border); border-radius: 8px; overflow: hidden; }
  .seg button { border: 0; border-radius: 0; background: var(--panel); padding: 4px 10px; font-size: 12px; }
  .seg button + button { border-left: 1px solid var(--border); }
  .seg .on { background: var(--text); color: var(--bg); }

  .body { display: grid; grid-template-columns: minmax(0, 1fr) 244px; min-height: 0; padding: 2px 0 2px var(--gutter); }
  .stage-col { display: flex; flex-direction: column; gap: 10px; padding: 12px 16px 8px; min-height: 0; min-width: 0; background: var(--surface); border: 1px solid var(--surface-border); border-radius: var(--surface-radius); box-shadow: var(--surface-shadow); }
  .stage { position: relative; flex: 1; min-height: 0; }
  .under { display: flex; justify-content: center; gap: 14px; font-size: 12px; min-height: 18px; }
  .banner { display: flex; align-items: center; gap: 10px; padding: 7px 10px 7px 12px; border-radius: 10px; background: color-mix(in srgb, var(--soft) 9%, var(--panel)); border: 1px solid color-mix(in srgb, var(--soft) 35%, var(--border)); font-size: 12.5px; }
  .banner b { color: var(--soft); }
  button.small { padding: 3px 9px; font-size: 12px; border-radius: 6px; }

  .chips-over { position: absolute; left: 12px; bottom: 10px; display: flex; gap: 6px; font-size: 11px; color: #fff; z-index: 3; pointer-events: none; flex-wrap: wrap; }
  .chips-over span { background: #000a; border-radius: 5px; padding: 2px 7px; }
  .chips-over .warn { background: var(--soft); }

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

  .pane { position: absolute; inset: 0; display: flex; flex-direction: column; gap: 10px; }
  .pane-h { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .tgrid { flex: 1; min-height: 0; overflow: auto; display: grid; grid-template-columns: repeat(auto-fill, minmax(118px, 1fr)); gap: 8px; align-content: start; padding: 4px; }
  .ti { position: relative; aspect-ratio: 3 / 2; padding: 0; border: 0; border-radius: 8px; overflow: visible; }
  .ti img { width: 100%; height: 100%; object-fit: cover; border-radius: 8px; filter: blur(1.4px); }
  .ti.off { opacity: 0.4; }
  .ti .score { position: absolute; left: 4px; bottom: 4px; font: 500 10px ui-monospace, 'SF Mono', monospace; background: #000b; color: #fff; border-radius: 4px; padding: 1px 5px; }
  .ti .x { position: absolute; right: -4px; bottom: -4px; width: 18px; height: 18px; border-radius: 50%; background: var(--dislike); color: #fff; font: 700 10px/18px system-ui; text-align: center; font-style: normal; border: 2px solid var(--bg); }
  .cmp { flex: 1; min-height: 0; display: grid; grid-template-columns: repeat(var(--n), minmax(0, 1fr)); gap: 10px; align-items: center; align-content: center; }
  .cf { position: relative; padding: 0; border: 0; border-radius: 10px; overflow: hidden; aspect-ratio: 3 / 2; box-shadow: 0 0 0 1px var(--border); }
  .cf img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; }
  .cf.best { box-shadow: 0 0 0 2px var(--like); }
  .cf .num { position: absolute; left: 6px; top: 6px; background: #000b; color: #fff; border-radius: 5px; padding: 0 6px; font-size: 11px; }
  .cf .lbl { position: absolute; inset: auto 0 0 0; padding: 14px 8px 6px; background: linear-gradient(transparent, #000c); color: #fff; font-size: 11px; display: grid; gap: 3px; text-align: left; }
  .meter { display: block; height: 6px; border-radius: 3px; background: var(--panel-2); overflow: hidden; }
  .meter b { display: block; height: 100%; background: var(--like); }
  .meter b.bad { background: var(--soft); }

  .insp { padding: 14px; display: flex; flex-direction: column; gap: 6px; overflow: hidden; }
  .kv { display: flex; justify-content: space-between; gap: 8px; color: var(--muted); font-size: 12.5px; border-top: 1px solid var(--border); padding-top: 7px; }
  .kv.first { border: 0; padding: 0; }
  .kv.col { flex-direction: column; gap: 5px; }
  .kv b { color: var(--text); font-weight: 500; font-family: ui-monospace, 'SF Mono', monospace; text-align: right; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .between { display: flex; justify-content: space-between; }
  .days-h { margin-top: 10px; }
  .days { display: flex; flex-direction: column; gap: 2px; overflow: hidden; }
  .dr { display: grid; grid-template-columns: 56px 1fr 48px; gap: 8px; align-items: center; padding: 4px 6px; border: 0; border-radius: 6px; background: transparent; font: 500 11.5px ui-monospace, 'SF Mono', monospace; text-align: left; }
  .dr:hover { background: var(--panel-2); }
  .dr.now { background: var(--panel-2); box-shadow: inset 2px 0 var(--accent); }
  .dr i { height: 5px; border-radius: 3px; background: var(--panel-2); overflow: hidden; display: block; }
  .dr i b { display: block; height: 100%; background: var(--done); }
  .r { text-align: right; }

  .bottom { padding: 8px 16px 10px; display: flex; flex-direction: column; gap: 9px; }
  .selbar { display: flex; align-items: center; gap: 8px; padding: 6px 8px 6px 12px; border-radius: 10px; background: color-mix(in srgb, var(--accent) 12%, var(--panel)); border: 1px solid color-mix(in srgb, var(--accent) 45%, var(--border)); font-size: 12.5px; flex-wrap: wrap; }
  .keys { display: flex; flex-wrap: wrap; gap: 4px 14px; font-size: 11.5px; justify-content: center; }

  /* Phone layout: swipe-first card stack, small action row. */
  .compact .top { padding-top: max(8px, env(safe-area-inset-top, 0px)); }
  .compact .body { display: flex; flex-direction: column; }
  .compact .body { padding: 0; }
  .compact .stage-col { flex: 1; padding: 8px 20px 0; background: none; border: 0; border-radius: 0; box-shadow: none; }
  .compact .bottom { padding-bottom: calc(10px + env(safe-area-inset-bottom, 0px)); }
  .compact .banner .grow, .compact .pane-h .grow { display: none; }
  .compact .banner { padding: 6px 8px 6px 10px; }
  .compact .banner b { flex: 1; }
  .actions { display: flex; gap: 8px; justify-content: center; align-items: center; }
  .round { width: 52px; height: 52px; border-radius: 50%; padding: 0; font-size: 22px; color: var(--c, var(--muted)); border: 1.5px solid var(--c, var(--border)); background: color-mix(in srgb, var(--c, var(--muted)) 10%, var(--panel)); }
  .round.sm { width: 38px; height: 38px; font-size: 16px; --c: var(--muted); }
  .round.like { --c: var(--like); } .round.dislike { --c: var(--dislike); } .round.favorite { --c: var(--favorite); } .round.hate { --c: var(--hate); }
  .burst { align-self: center; }
</style>
