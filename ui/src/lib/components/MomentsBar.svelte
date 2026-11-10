<script lang="ts">
  import type { Group, Photo } from '../api';
  import { short } from '../days';
  import { formatTime } from '../format';
  import { dragSelect } from '../select';

  let {
    photos,
    moments,
    idx,
    selection,
    compact = false,
    onSelect,
    onJump,
  }: {
    photos: Photo[];
    moments: Group[];
    idx: number;
    selection: { from: number; to: number } | null;
    compact?: boolean;
    onSelect: (from: number, to: number) => void;
    onJump: (photoIndex: number) => void;
  } = $props();

  const rows = $derived(
    moments.map((m, i) => {
      let done = 0;
      for (let k = m.start; k < m.end; k++) {
        if (photos[k].decision) done++;
      }
      const n = m.end - m.start;
      const first = photos[m.start];
      const day = first.capturedAt?.slice(0, 10) ?? '';
      return {
        i,
        n,
        done,
        day,
        now: idx >= m.start && idx < m.end,
        pos: idx >= m.start && idx < m.end ? ((idx - m.start + 0.5) / n) * 100 : 0,
        selected: !!selection && selection.from <= m.start && m.end - 1 <= selection.to,
        title: `${first.capturedAt ? short(day) : ''} ${formatTime(first.capturedAt)} · ${n} photos · ${done} done`,
        newDay: i === 0 || photos[moments[i - 1].start].capturedAt?.slice(0, 10) !== day,
      };
    }),
  );

  function jump(i: number) {
    const m = moments[i];
    let k = m.start;
    while (k < m.end - 1 && photos[k].decision) k++;
    onJump(k);
  }
</script>

<!-- One block per moment: width grows with photo count, fill is progress. -->
<div
  class="bar"
  class:compact
  role="group"
  aria-label="Moments timeline"
  use:dragSelect={{ onSelect: (a, b) => onSelect(moments[a].start, moments[b].end - 1), onClick: jump }}
>
  {#each rows as r (r.i)}
    {#if r.newDay && !compact}<span class="day">{r.day ? short(r.day) : ''}</span>{/if}
    <span
      class="m"
      class:now={r.now}
      class:selected={r.selected}
      data-i={r.i}
      style:flex="{Math.sqrt(r.n).toFixed(2)} 1 0"
      style:--f="{(r.done / r.n) * 100}%"
      title={r.title}
    >
      {#if r.now}<u style:left="{r.pos}%"></u>{/if}
    </span>
  {/each}
</div>

<style>
  .bar { display: flex; gap: 2px; align-items: center; padding-top: 7px; user-select: none; touch-action: none; }
  .day { flex: none; font: 500 10px ui-monospace, 'SF Mono', monospace; color: var(--muted); padding: 0 5px 0 7px; margin-left: 3px; border-left: 1px solid var(--border); align-self: stretch; display: flex; align-items: center; }
  .day:first-child { border: 0; margin: 0; padding-left: 0; }
  .m {
    --f: 0%;
    flex: 1 1 0;
    min-width: 3px;
    height: 26px;
    border-radius: 3px;
    position: relative;
    cursor: pointer;
    background: linear-gradient(90deg, var(--done) var(--f), color-mix(in srgb, var(--text) 16%, transparent) var(--f));
  }
  .compact .m { height: 14px; }
  .m.now { box-shadow: inset 0 0 0 1.5px var(--accent); }
  .m.selected { outline: 2px solid var(--accent); outline-offset: 1px; }
  u { position: absolute; top: -3px; bottom: -3px; width: 3px; margin-left: -1px; background: var(--accent); border-radius: 2px; box-shadow: 0 0 0 2px var(--bg); pointer-events: none; }
</style>
