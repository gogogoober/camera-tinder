<script module lang="ts">
  import type { RescueMark } from '../triage';
  // Marks survive leaving the screen (Esc, back to Cull) for the session; only Done applies them.
  const remembered = new Map<number, RescueMark>();
</script>

<script lang="ts">
  import { onMount } from 'svelte';
  import { api, type Decision, type OutcomeEntry, type Photo, type TagSuggestion, type TriageItem } from '../api';
  import type { IssueKind } from '../api/analysisTypes';
  import { DECISIONS } from '../decisions';
  import { daysLabel, short } from '../days';
  import { formatCount, previewEdge } from '../format';
  import { ISSUE_LABELS, matchesChips, nextRescueMark } from '../triage';
  import { viewport } from '../viewport.svelte';
  import AppShell from './layout/AppShell.svelte';

  // The end-of-session screen for photos that looked bad ("set aside"): one grid, neutral, nothing
  // X'd out. Click cycles ✓ Keep → ★ Favorite → ⊘ Hate → clear. Done decides all of them at once:
  // marked ones as marked, the rest as ✕ Nope. Leaving without Done decides nothing.
  let { days, onBack, onDone }: {
    /** Exactly the days picked on the calendar (as Cull got them). */
    days: string[];
    onBack: () => void;
    onDone: () => void;
  } = $props();

  interface Row { photo: Photo; item: TriageItem; day: string }

  const edge = previewEdge();
  const LONG_PRESS_MS = 450;

  let rows = $state<Row[]>([]);
  /** Calendar events touching the picked days (the old saved tags are gone; events come from suggestions). */
  let events = $state<TagSuggestion[]>([]);
  let triageVersion = '';
  let marks = $state(new Map<number, RescueMark>(remembered));
  let chosenEvents = $state(new Set<string>());
  let chosenReasons = $state(new Set<IssueKind>());
  let enlarged = $state<Photo | null>(null);
  let hovered: number | null = null;
  let saving = $state(false);
  let loading = $state(true);
  let error = $state<string | null>(null);
  const compact = $derived(viewport.compact);

  onMount(async () => {
    try {
      const [plan, suggestions] = await Promise.all([api.plan({ days, status: 'all' }), api.suggestTags().catch(() => [] as TagSuggestion[])]);
      triageVersion = plan.triageVersion;
      // Calendar events (and the events nested inside them) that cover at least one picked day.
      events = suggestions
        .flatMap((s) => [s, ...s.children])
        .filter((e) => e.source === 'calendar' && days.some((d) => d >= e.startDay && d <= e.endDay));
      rows = plan.photos
        .map((photo, i) => ({ photo, item: plan.triage[i], day: photo.capturedAt?.slice(0, 10) ?? 'unknown' }))
        .filter((r) => r.item?.question === 'sweep' && !r.photo.decision);
      const ids = new Set(rows.map((r) => r.photo.id));
      marks = new Map([...marks].filter(([id]) => ids.has(id))); // forget marks for photos decided elsewhere
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  });

  // Top chip row: calendar events that touch the picked days; days no event covers get a chip each.
  interface EventChip { key: string; label: string; matches: (day: string) => boolean }
  const eventChips = $derived.by((): EventChip[] => {
    const photoDays = [...new Set(rows.map((r) => r.day))].sort();
    const covered = (e: TagSuggestion, d: string) => d >= e.startDay && d <= e.endDay;
    const chips: EventChip[] = events.map((e) => ({ key: `event:${e.id}`, label: e.name, matches: (d) => covered(e, d) }));
    const untagged = photoDays.filter((d) => !events.some((e) => covered(e, d)));
    return [...chips, ...untagged.map((d): EventChip => ({ key: `day:${d}`, label: d === 'unknown' ? 'No date' : short(d), matches: (x) => x === d }))].filter((c) =>
      rows.some((r) => c.matches(r.day)),
    );
  });
  // Bottom chip row: the worst reason each photo was set aside for.
  const worst = (r: Row): IssueKind | undefined => r.item.reasons[0];
  const reasonChips = $derived([...new Set(rows.map(worst).filter((k): k is IssueKind => !!k))]);

  const visible = $derived(
    rows.filter((r) => {
      const ev = eventChips.filter((c) => chosenEvents.has(c.key));
      return (ev.length === 0 || ev.some((c) => c.matches(r.day))) && matchesChips(chosenReasons, worst(r));
    }),
  );

  const finalDecision = (r: Row): Decision => marks.get(r.photo.id) ?? 'dislike';
  const tally = $derived.by(() => {
    const n: Record<Decision, number> = { like: 0, favorite: 0, hate: 0, dislike: 0 };
    for (const r of rows) n[finalDecision(r)]++;
    return n;
  });
  const doneLabel = $derived(
    (['like', 'favorite', 'hate', 'dislike'] as Decision[])
      .filter((d) => tally[d] > 0 || d === 'dislike')
      .map((d) => `${tally[d]} ${DECISIONS[d].label}`)
      .join(', '),
  );

  function toggle<T>(set: Set<T>, v: T): Set<T> {
    const next = new Set(set);
    if (!next.delete(v)) next.add(v);
    return next;
  }

  // Long-press (phone) enlarges a photo; so does space (Mac) on the hovered one.
  let timer: ReturnType<typeof setTimeout> | undefined;
  let pressed = false;

  function click(r: Row) {
    if (pressed) return; // that click was the end of a long press
    const next = nextRescueMark(marks.get(r.photo.id));
    const m = new Map(marks);
    if (next) m.set(r.photo.id, next);
    else m.delete(r.photo.id);
    marks = m;
    remembered.clear();
    m.forEach((v, k) => remembered.set(k, v));
  }

  function press(r: Row) {
    pressed = false;
    timer = setTimeout(() => {
      pressed = true;
      enlarged = r.photo;
    }, LONG_PRESS_MS);
  }
  function release() {
    clearTimeout(timer);
    setTimeout(() => (pressed = false), 0);
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      if (enlarged) enlarged = null;
      else onBack();
    } else if (e.key === ' ') {
      e.preventDefault();
      enlarged = enlarged ? null : (rows.find((r) => r.photo.id === hovered)?.photo ?? null);
    }
  }

  async function done() {
    saving = true;
    try {
      const outcomes: OutcomeEntry[] = rows.map((r) => ({ item: r.item, planVersion: triageVersion, place: 'rescue', decision: finalDecision(r) }));
      for (const d of ['like', 'favorite', 'hate', 'dislike'] as Decision[]) {
        const ids = rows.filter((r) => finalDecision(r) === d).map((r) => r.photo.id);
        if (ids.length) await api.decideMany(ids, d);
      }
      await api.logOutcomes(outcomes);
      remembered.clear();
      onDone();
    } catch (e) {
      error = String(e);
      saving = false;
    }
  }
</script>

<svelte:window onkeydown={onKey} />

{#snippet topBar()}
  <header class="top">
    <button onclick={onBack}>{compact ? 'Cull' : '‹ Back to cull'}</button>
    <b>Rescue · {daysLabel(days)}</b>
    <span class="grow"></span>
    {#if !compact}<span class="muted">Click: ✓ Keep → ★ Favorite → ⊘ Hate → clear · <kbd>space</kbd> enlarge</span>{/if}
  </header>
{/snippet}

<AppShell top={topBar}>
  <section class="main">
    {#if loading}
      <p class="muted">Loading…</p>
    {:else if rows.length === 0}
      <div class="empty">
        <b>Nothing is waiting here.</b>
        <button class="primary" onclick={onDone}>Review &amp; export</button>
      </div>
    {:else}
      <p class="lede">{formatCount(rows.length)} photos looked bad. Rescue any you want; the rest become ✕ Nope (nothing is deleted).</p>
      <div class="chiprow"><span class="lbl">Events</span>
        <div class="chips">
          {#each eventChips as c (c.key)}
            <button class="chip" class:on={chosenEvents.has(c.key)} onclick={() => (chosenEvents = toggle(chosenEvents, c.key))}>
              {c.label} <span class="mono">{rows.filter((r) => c.matches(r.day)).length}</span>
            </button>
          {/each}
        </div>
      </div>
      <div class="chiprow"><span class="lbl">Issues</span>
        <div class="chips">
          {#each reasonChips as k (k)}
            <button class="chip" class:on={chosenReasons.has(k)} onclick={() => (chosenReasons = toggle(chosenReasons, k))}>
              {ISSUE_LABELS[k]} <span class="mono">{rows.filter((r) => worst(r) === k).length}</span>
            </button>
          {/each}
        </div>
      </div>

      {#if visible.length}
        <div class="grid">
          {#each visible as r (r.photo.id)}
            {@const mark = marks.get(r.photo.id)}
            <button
              class="cell {mark ?? ''}"
              onclick={() => click(r)}
              onpointerdown={() => press(r)}
              onpointerup={release}
              onpointerleave={release}
              onmouseenter={() => (hovered = r.photo.id)}
              onmouseleave={() => (hovered = null)}
              aria-label="{r.photo.fileName}: {r.item.reasons.map((k) => ISSUE_LABELS[k]).join(', ')}"
            >
              <img src={api.previewUrl(r.photo.id, 256)} alt="" draggable="false" loading="lazy" />
              {#if worst(r)}<span class="why">{ISSUE_LABELS[worst(r)!]}</span>{/if}
              {#if mark}<span class="mark" style:background="var(--{mark})">{DECISIONS[mark].icon} {DECISIONS[mark].label}</span>{/if}
            </button>
          {/each}
        </div>
      {:else}
        <p class="muted empty">Nothing set aside matches these filters.</p>
      {/if}

      <footer class="foot">
        <span class="muted">{chosenEvents.size || chosenReasons.size ? `Filtered view. Done still applies to all ${rows.length}.` : 'No chip selected = everything.'} Leaving without Done decides nothing.</span>
        <button class="primary" onclick={done} disabled={saving}>{saving ? 'Saving…' : `Done: ${doneLabel}`}</button>
      </footer>
    {/if}
    {#if error}<p class="error">{error}</p>{/if}
  </section>
</AppShell>

{#if enlarged}
  <button class="overlay" onclick={() => (enlarged = null)} aria-label="Close enlarged photo">
    <img src={api.previewUrl(enlarged.id, edge)} alt="" />
  </button>
{/if}

<style>
  .top { display: flex; align-items: center; gap: 12px; padding: 0 4px; }
  .grow { flex: 1; }
  .main { display: flex; flex-direction: column; gap: 12px; padding: 16px; overflow-y: auto; min-height: 0; }
  .lede { margin: 0; }
  .chiprow { display: flex; gap: 10px; align-items: baseline; }
  .lbl { flex: none; width: 54px; font: 600 10px ui-monospace, 'SF Mono', monospace; letter-spacing: 0.08em; text-transform: uppercase; color: var(--muted); }
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(104px, 1fr)); gap: 10px; }
  .cell { position: relative; aspect-ratio: 1; padding: 0; border: 0; border-radius: 9px; overflow: hidden; background: var(--panel-2); cursor: pointer; }
  .cell img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; pointer-events: none; }
  .cell::after { content: ''; position: absolute; inset: 0; border-radius: inherit; pointer-events: none; }
  .cell.like::after { box-shadow: inset 0 0 0 4px var(--like); }
  .cell.favorite::after { box-shadow: inset 0 0 0 4px var(--favorite); }
  .cell.hate::after { box-shadow: inset 0 0 0 4px var(--hate); }
  .why, .mark { position: absolute; z-index: 1; font: 600 10px/1 ui-monospace, 'SF Mono', monospace; padding: 4px 6px; border-radius: 99px; color: #fff; }
  .why { left: 6px; bottom: 6px; background: #000a; }
  .mark { right: 6px; top: 6px; }
  .foot { display: flex; justify-content: space-between; gap: 12px; align-items: center; flex-wrap: wrap; position: sticky; bottom: 0; padding: 10px 0 0; background: var(--surface); font-size: 13px; }
  .empty { display: grid; gap: 12px; justify-items: center; padding: 40px 0; text-align: center; }
  .overlay { position: fixed; inset: 0; z-index: 50; display: grid; place-items: center; padding: 24px; border: 0; border-radius: 0; background: #000d; cursor: zoom-out; }
  .overlay img { max-width: 100%; max-height: 100%; object-fit: contain; border-radius: 8px; }
</style>
