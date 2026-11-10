<script lang="ts">
  import type { DaySummary } from '../api';
  import { MONTHS, dayKey, heat, parseDay } from '../days';
  import { dragSelect } from '../select';

  /**
   * Full-month calendars in one horizontal row, heat-coloured by photos still to cull.
   * `selected` (a set of `YYYY-MM-DD`) is the one source of truth for what gets culled.
   * Click a day to toggle it; drag to add days, or drag from a selected day to remove them.
   * Only days with photos can be selected.
   */
  let {
    days,
    selected,
    onChange,
  }: {
    days: DaySummary[];
    selected: Set<string>;
    onChange: (next: Set<string>) => void;
  } = $props();

  let rowEl = $state<HTMLElement>();
  /** The newest month we last scrolled to; a scan adding a newer month scrolls again. */
  let positioned = '';

  const byDay = $derived(new Map(days.filter((d) => d.day).map((d) => [d.day as string, d])));
  const dated = $derived(days.filter((d) => d.day).map((d) => d.day as string).sort());
  const hasPhotos = (d: string) => (byDay.get(d)?.stats.total ?? 0) > 0;

  /** Every month from the first photo to the last, complete. `cells` indexes every day shown. */
  const view = $derived.by(() => {
    const months: { title: string; blanks: number; days: { day: string; i: number }[] }[] = [];
    const cells: string[] = [];
    if (!dated.length) return { months, cells };
    const a = parseDay(dated[0]);
    const b = parseDay(dated[dated.length - 1]);
    for (let y = a.getFullYear(), m = a.getMonth(); y * 12 + m <= b.getFullYear() * 12 + b.getMonth(); m++) {
      if (m > 11) {
        m = 0;
        y++;
      }
      const dim = new Date(y, m + 1, 0).getDate();
      const list = Array.from({ length: dim }, (_, k) => {
        const day = dayKey(new Date(y, m, k + 1));
        cells.push(day);
        return { day, i: cells.length - 1 };
      });
      months.push({ title: `${MONTHS[m]} ${y}`, blanks: new Date(y, m, 1).getDay(), days: list });
    }
    return { months, cells };
  });

  // Open on the newest month.
  $effect(() => {
    const newest = view.months.at(-1)?.title ?? '';
    if (!rowEl || !newest || positioned === newest) return;
    const el = rowEl;
    requestAnimationFrame(() => (el.scrollLeft = el.scrollWidth));
    positioned = newest;
  });

  // A drag adds days, or removes them when it starts on a selected day. It applies to the
  // selection as it was when the drag began, so shrinking the drag undoes days again.
  let base: Set<string> = new Set();
  let erase = false;
  function start(i: number) {
    base = new Set(selected);
    erase = selected.has(view.cells[i]);
  }
  function drag(from: number, to: number) {
    const next = new Set(base);
    for (const d of view.cells.slice(from, to + 1)) {
      if (!hasPhotos(d)) continue;
      if (erase) next.delete(d);
      else next.add(d);
    }
    onChange(next);
  }
  function click(i: number) {
    const d = view.cells[i];
    if (!hasPhotos(d)) return;
    const next = new Set(selected);
    if (!next.delete(d)) next.add(d);
    onChange(next);
  }

  function tip(d: string) {
    const s = byDay.get(d);
    return s?.stats.total ? `${d} · ${s.stats.total} photos · ${s.stats.undecided} to cull` : `${d} · no photos`;
  }
</script>

<div class="row" bind:this={rowEl} use:dragSelect={{ onSelect: drag, onClick: click, onStart: start }}>
  {#each view.months as m (m.title)}
    <section>
      <h5>{m.title}</h5>
      <div class="grid">
        {#each ['S', 'M', 'T', 'W', 'T', 'F', 'S'] as w, i (i)}<span class="dow">{w}</span>{/each}
        {#each { length: m.blanks } as _, i (i)}<span></span>{/each}
        {#each m.days as { day, i } (day)}
          {@const s = byDay.get(day)}
          <button class="cell {heat(s)}" class:sel={selected.has(day)} data-i={i} title={tip(day)} aria-label={tip(day)} aria-pressed={selected.has(day)} tabindex="-1">
            <span class="d">{parseDay(day).getDate()}</span>
            {#if s && s.stats.total}<span class="n">{s.stats.total}</span>{/if}
          </button>
        {/each}
      </div>
    </section>
  {/each}
</div>

<style>
  .row { display: flex; gap: 22px; overflow-x: auto; padding: 2px 2px 8px; user-select: none; scroll-snap-type: x proximity; }
  section { flex: none; scroll-snap-align: start; }
  h5 { margin: 0 0 6px; font-size: 13px; font-weight: 600; }
  .grid { --cs: 38px; display: grid; grid-template-columns: repeat(7, var(--cs)); grid-template-rows: 14px; grid-auto-rows: var(--cs); gap: 3px; }
  .dow { font: 500 10px ui-monospace, 'SF Mono', monospace; color: var(--muted); text-align: center; align-self: end; }

  .cell {
    --c: 0%;
    position: relative;
    padding: 0;
    border: 0;
    border-radius: 6px;
    background: color-mix(in srgb, var(--accent) var(--c), var(--cell0));
    color: var(--text);
    cursor: pointer;
  }
  .cell:hover:not(:disabled) { background: color-mix(in srgb, var(--accent) var(--c), var(--cell0)); filter: brightness(1.12); border: 0; }
  .cell.l1 { --c: 16%; }
  .cell.l2 { --c: 36%; }
  .cell.l3 { --c: 62%; }
  .cell.l4 { --c: 94%; }
  .cell.l3, .cell.l4 { color: var(--accent-fg); }
  .cell.none { background: transparent; box-shadow: inset 0 0 0 1px var(--border); color: var(--muted); cursor: default; opacity: 0.6; }
  .cell.none:hover { filter: none; }
  .cell.done { background: transparent; box-shadow: inset 0 0 0 1.5px var(--done); }

  /* Day number always top-left; photo count always bottom-right, in a pill. */
  .d { position: absolute; top: 3px; left: 5px; font: 500 11px ui-monospace, 'SF Mono', monospace; line-height: 1; }
  .n {
    position: absolute;
    right: 2px;
    bottom: 2px;
    min-width: 15px;
    height: 15px;
    padding: 0 4px;
    box-sizing: border-box;
    border-radius: 8px;
    background: var(--bg);
    color: var(--text);
    font: 600 9px ui-monospace, 'SF Mono', monospace;
    line-height: 15px;
    text-align: center;
  }

  /* Drawn inside the cell, so it never spills onto neighbouring days. */
  .cell.sel { box-shadow: inset 0 0 0 2px var(--text), inset 0 0 0 3.5px var(--bg); }
</style>
