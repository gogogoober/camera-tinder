<script lang="ts">
  import type { DaySummary, Tag, TagSuggestion } from '../api';
  import { MONTHS, addDays, dayKey, diffDays, heat, parseDay } from '../days';
  import { dragSelect } from '../select';

  type Range = { from: string; to: string };
  type Labeled = { name: string; startDay: string; endDay: string; source: string };

  let {
    days,
    tags,
    suggestions,
    range,
    layout,
    onRange,
    onTag,
  }: {
    days: DaySummary[];
    tags: Tag[];
    suggestions: TagSuggestion[];
    range: Range | null;
    layout: 'weeks' | 'months';
    onRange: (r: Range) => void;
    onTag: (t: Labeled) => void;
  } = $props();

  let weeksEl = $state<HTMLElement>();
  let monthsEl = $state<HTMLElement>();
  let positioned = false;

  // Start where the action is: latest weeks on the Mac, the selected range on phones.
  $effect(() => {
    if (positioned || !grid.weeks) return;
    if (layout === 'weeks' && weeksEl) {
      weeksEl.scrollLeft = weeksEl.scrollWidth;
      positioned = true;
    } else if (layout === 'months' && monthsEl && range) {
      monthsEl.querySelector('.edge')?.scrollIntoView({ block: 'center' });
      positioned = true;
    }
  });

  const SRC: Record<string, string> = { manual: '#', calendar: '▦', place: '⌖', gap: '⋯', image: '◐' };
  const MAX_WEEKS = 53;

  const byDay = $derived(new Map(days.filter((d) => d.day).map((d) => [d.day as string, d])));
  const dated = $derived(days.filter((d) => d.day).map((d) => d.day as string));

  /** First Sunday-start week and the number of weeks to show (latest weeks win). */
  const grid = $derived.by(() => {
    if (!dated.length) return { first: '', weeks: 0, last: '' };
    const last = dated[dated.length - 1];
    const lastD = parseDay(last);
    const lastWeekStart = new Date(lastD.getFullYear(), lastD.getMonth(), lastD.getDate() - lastD.getDay());
    const firstD = parseDay(dated[0]);
    const firstWeekStart = new Date(firstD.getFullYear(), firstD.getMonth(), firstD.getDate() - firstD.getDay());
    const weeks = Math.min(MAX_WEEKS, Math.round((lastWeekStart.getTime() - firstWeekStart.getTime()) / (7 * 86_400_000)) + 1);
    const start = new Date(lastWeekStart.getFullYear(), lastWeekStart.getMonth(), lastWeekStart.getDate() - (weeks - 1) * 7);
    return { first: dayKey(start), weeks, last };
  });

  const cells = $derived(Array.from({ length: grid.weeks * 7 }, (_, i) => addDays(grid.first, i)));
  const inRange = (d: string) => !!range && d >= range.from && d <= range.to;

  // Month view: one block per month.
  const months = $derived.by(() => {
    if (!grid.weeks) return [];
    const out: { title: string; blanks: number; days: string[] }[] = [];
    const a = parseDay(grid.first);
    const b = parseDay(grid.last);
    for (let y = a.getFullYear(), m = a.getMonth(); y < b.getFullYear() || (y === b.getFullYear() && m <= b.getMonth()); m++) {
      if (m > 11) { m = 0; y++; }
      const dim = new Date(y, m + 1, 0).getDate();
      const list = Array.from({ length: dim }, (_, k) => dayKey(new Date(y, m, k + 1)));
      out.push({ title: `${MONTHS[m]} ${y}`, blanks: new Date(y, m, 1).getDay(), days: list });
      if (y === b.getFullYear() && m === b.getMonth()) break;
    }
    return out;
  });

  // Drag across cells in either layout. `data-i` is the index into `cells`.
  function select(from: number, to: number) {
    onRange({ from: cells[from], to: cells[to] });
  }
  function click(i: number, e: PointerEvent) {
    const d = cells[i];
    if (e.shiftKey && range) {
      onRange({ from: d < range.from ? d : range.from, to: d > range.to ? d : range.to });
    } else onRange({ from: d, to: d });
  }
  const monthIndex = (day: string) => diffDays(grid.first, day);

  // Tag lanes above the weeks grid: a bar per tag or suggestion, packed into rows.
  const lanes = $derived.by(() => {
    if (layout !== 'weeks' || !grid.weeks) return [];
    const items = [
      ...tags.map((t) => ({ ...t, kind: 'tag' as const })),
      ...suggestions.map((s) => ({ ...s, kind: 'suggestion' as const })),
    ];
    const placed: { item: (typeof items)[number]; a: number; b: number }[][] = [];
    for (const item of items) {
      const lo = diffDays(grid.first, item.startDay);
      const hi = diffDays(grid.first, item.endDay);
      if (hi < 0 || lo >= grid.weeks * 7) continue;
      const a = Math.max(0, Math.floor(lo / 7));
      const natural = Math.floor(Math.min(hi, grid.weeks * 7 - 1) / 7);
      const b = Math.min(grid.weeks - 1, Math.max(natural, a + Math.ceil((item.name.length * 6.6 + 30) / 25) - 1));
      let lane = placed.find((l) => l.every((x) => b < x.a || a > x.b));
      if (!lane) {
        if (placed.length >= 3) continue;
        lane = [];
        placed.push(lane);
      }
      lane.push({ item, a, b });
    }
    return placed;
  });

  const monthLabels = $derived(
    Array.from({ length: grid.weeks }, (_, w) => {
      const d = parseDay(addDays(grid.first, w * 7));
      return d.getDate() <= 7 ? { col: w + 1, text: MONTHS[d.getMonth()] } : null;
    }).filter((x) => x !== null),
  );

  const tagged = $derived(new Set(tags.flatMap((t) => cells.filter((c) => c >= t.startDay && c <= t.endDay))));

  function tip(d: string) {
    const s = byDay.get(d);
    return s ? `${d} · ${s.stats.total} photos · ${s.stats.undecided} to cull` : d;
  }
  const isActive = (t: Labeled) => !!range && range.from === t.startDay && range.to === t.endDay;
</script>

{#if layout === 'weeks'}
  <div class="weeks" style:--weeks={grid.weeks} bind:this={weeksEl}>
    <div class="row months">
      {#each monthLabels as m (m.col)}<span style:grid-column={m.col}>{m.text}</span>{/each}
    </div>
    {#each lanes as lane, li (li)}
      <div class="row lane">
        {#each lane as p (p.item.kind + p.item.name + p.item.startDay)}
          <button
            class="tagbar {p.item.kind}"
            class:on={isActive(p.item)}
            style:grid-column="{p.a + 1} / {p.b + 2}"
            title="{p.item.kind === 'suggestion' ? 'Suggested · ' : ''}{p.item.name}"
            onclick={() => onTag(p.item)}
          >
            <span class="src">{SRC[p.item.source] ?? '#'}</span>{p.item.name}
          </button>
        {/each}
      </div>
    {/each}
    <div class="row cells" use:dragSelect={{ onSelect: select, onClick: click }}>
      {#each cells as d, i (d)}
        <button
          class="cell {heat(byDay.get(d))}"
          class:in={inRange(d)}
          class:edge={range && (d === range.from || d === range.to)}
          data-i={i}
          title={tip(d)}
          aria-label={tip(d)}
          tabindex="-1"
        ></button>
      {/each}
    </div>
  </div>
{:else}
  <div class="months-view" bind:this={monthsEl} use:dragSelect={{ onSelect: (a, b) => onRange({ from: cells[a], to: cells[b] }), onClick: click }}>
    <div class="dow">{#each ['S', 'M', 'T', 'W', 'T', 'F', 'S'] as w, i (i)}<span>{w}</span>{/each}</div>
    {#each months as m (m.title)}
      <section>
        <h5>{m.title}</h5>
        <div class="mgrid">
          {#each { length: m.blanks } as _, i (i)}<span></span>{/each}
          {#each m.days as d (d)}
            {@const s = byDay.get(d)}
            <button
              class="cell {heat(s)}"
              class:in={inRange(d)}
              class:edge={range && (d === range.from || d === range.to)}
              class:tagged={tagged.has(d)}
              data-i={monthIndex(d)}
              title={tip(d)}
              tabindex="-1"
            >
              {parseDay(d).getDate()}
              {#if s && s.stats.total}<small>{s.stats.undecided || '✓'}</small>{/if}
            </button>
          {/each}
        </div>
      </section>
    {/each}
  </div>
{/if}

<style>
  .weeks { --cs: 22px; overflow-x: auto; padding-bottom: 6px; }
  .row { display: grid; gap: 3px; grid-template-columns: repeat(var(--weeks), var(--cs)); }
  .months { height: 18px; font: 500 10.5px ui-monospace, 'SF Mono', monospace; color: var(--muted); }
  .months span { white-space: nowrap; }
  .lane { height: 20px; margin-bottom: 3px; }
  .cells { grid-template-rows: repeat(7, var(--cs)); grid-auto-flow: column; grid-auto-columns: var(--cs); grid-template-columns: none; touch-action: none; user-select: none; }

  .tagbar {
    grid-row: 1;
    display: flex;
    gap: 4px;
    align-items: center;
    font-size: 11px;
    padding: 0 6px;
    border-radius: 5px;
    overflow: hidden;
    white-space: nowrap;
    background: color-mix(in srgb, var(--text) 9%, var(--bg));
  }
  .tagbar.suggestion { background: transparent; border-style: dashed; color: var(--muted); }
  .tagbar.on { background: var(--text); color: var(--bg); border-color: var(--text); }
  .src { font: 11px ui-monospace, 'SF Mono', monospace; opacity: 0.8; }

  .cell {
    --c: 0%;
    position: relative;
    padding: 0;
    border: 0;
    border-radius: 4px;
    background: color-mix(in srgb, var(--accent) var(--c), var(--cell0));
    cursor: pointer;
  }
  .cell:hover:not(:disabled) { background: color-mix(in srgb, var(--accent) var(--c), var(--cell0)); filter: brightness(1.12); border: 0; }
  .cell.l1 { --c: 16%; }
  .cell.l2 { --c: 36%; }
  .cell.l3 { --c: 62%; }
  .cell.l4 { --c: 94%; }
  .cell.none { background: transparent; box-shadow: inset 0 0 0 1px var(--border); opacity: 0.6; }
  .cell.done { background: transparent; box-shadow: inset 0 0 0 1.5px var(--done); }
  .cell.done::after {
    content: '';
    position: absolute;
    inset: 0;
    margin: auto;
    width: 35%;
    height: 20%;
    border-left: 1.5px solid var(--done);
    border-bottom: 1.5px solid var(--done);
    transform: translateY(-25%) rotate(-45deg);
  }
  .cell.in { box-shadow: inset 0 0 0 2px var(--text); }
  .cell.edge { box-shadow: 0 0 0 2px var(--bg), 0 0 0 4px var(--accent); z-index: 1; }

  /* Month calendar (phones). */
  .months-view { touch-action: none; user-select: none; }
  .dow { display: grid; grid-template-columns: repeat(7, 1fr); text-align: center; font: 500 10px ui-monospace, 'SF Mono', monospace; color: var(--muted); padding-bottom: 4px; position: sticky; top: 0; background: var(--bg); z-index: 2; }
  section { margin-bottom: 16px; }
  h5 { margin: 0 0 6px; font-size: 15px; }
  .mgrid { display: grid; grid-template-columns: repeat(7, 1fr); gap: 4px; }
  .mgrid .cell { height: 44px; border-radius: 8px; display: flex; flex-direction: column; align-items: center; justify-content: center; font: 500 12px ui-monospace, 'SF Mono', monospace; line-height: 1.1; }
  .mgrid .cell small { font-size: 8.5px; opacity: 0.8; }
  .mgrid .cell.done::after { display: none; }
  .mgrid .cell.l3, .mgrid .cell.l4 { color: var(--accent-fg); }
  .mgrid .cell.tagged::before { content: ''; position: absolute; left: 8px; right: 8px; bottom: 3px; height: 2px; border-radius: 1px; background: var(--text); opacity: 0.55; }
</style>
