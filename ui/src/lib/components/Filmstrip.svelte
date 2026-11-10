<script lang="ts">
  import { api, type Photo, type TriageItem } from '../api';
  import { formatTime } from '../format';
  import { dragSelect } from '../select';
  import { ISSUE_LABELS, drawnItems, groupOf, stopOf, stripWindow, type Structure } from '../triage';
  import Badge from './Badge.svelte';

  let {
    photos,
    triage,
    s,
    idx,
    selection,
    momentOf,
    back,
    forward,
    size,
    onSelect,
    onJump,
  }: {
    photos: Photo[];
    triage: TriageItem[];
    /** Shape of the plan (groups, set-aside photos); see lib/triage.ts. */
    s: Structure;
    /** The photo shown right now (a group member, or a set-aside photo opened early). */
    idx: number;
    selection: { from: number; to: number } | null;
    momentOf: number[];
    back: number;
    forward: number;
    size: number;
    onSelect: (from: number, to: number) => void;
    onJump: (photoIndex: number) => void;
  } = $props();

  const SLICES = 5;

  // Everything the strip draws, in capture order: a photo, a burst/group stack (at its first member), or
  // a thin red sliver for a set-aside photo. Other group members live inside their stack, so the window
  // is counted in drawn items, not photos: a 20-frame burst is one item, not 20 hidden slots.
  const drawn = $derived(drawnItems(s, photos.length));
  const kindOf = (i: number) => (s.setAside[i] ? ('sweep' as const) : s.groupAt[i] == null ? ('single' as const) : ('stack' as const));

  const items = $derived(
    stripWindow(s, drawn, idx, back, forward).map((i, k, all) => {
      if (i === undefined) return { key: `empty${k}`, i: -1, p: null, kind: 'empty' as const, divider: false };
      const prev = all[k - 1];
      return { key: `p${i}`, i, p: photos[i], kind: kindOf(i), divider: prev !== undefined && momentOf[prev] !== momentOf[i] };
    }),
  );

  const isCurrent = (i: number, kind: string) => (kind === 'stack' ? stopOf(s, idx) === i : i === idx);
  const isSelected = (i: number) => !!selection && i >= selection.from && i <= selection.to;
  const sweepTitle = (i: number) =>
    `Set aside · ${triage[i]?.reasons.map((r) => ISSUE_LABELS[r]).join(', ') || 'looks bad'} · ${formatTime(photos[i].capturedAt)}. Click to open`;
</script>

<div class="strip" style:--ts="{size}px" role="group" aria-label="Filmstrip" use:dragSelect={{ onSelect, onClick: (i) => onJump(i) }}>
  {#each items as it (it.key)}
    {#if it.divider}<span class="div"></span>{/if}
    {#if it.kind === 'sweep'}
      <span
        class="sw"
        class:cur={isCurrent(it.i, it.kind)}
        class:done={!!it.p!.decision}
        class:selected={isSelected(it.i)}
        data-i={it.i}
        role="button"
        tabindex="-1"
        aria-label="Set-aside photo"
        title={sweepTitle(it.i)}
      ></span>
    {:else if it.kind === 'stack'}
      {@const members = groupOf(s, it.i)}
      <span
        class="stack"
        class:cur={isCurrent(it.i, it.kind)}
        class:past={it.i < idx && !isCurrent(it.i, it.kind)}
        class:selected={isSelected(it.i)}
        data-i={it.i}
        title="{triage[it.i]?.group?.kind === 'burst' ? 'Burst' : 'Similar photos'} of {members.length} · {formatTime(it.p!.capturedAt)}"
      >
        <i class="bn">×{members.length}</i>
        {#each members.slice(0, SLICES) as m, k (m)}
          <span class="sl" class:last={k === Math.min(members.length, SLICES) - 1}>
            <img src={api.previewUrl(photos[m].id, 256)} alt="" draggable="false" loading="lazy" />
          </span>
        {/each}
      </span>
    {:else if it.p}
      <span
        class="th"
        class:cur={isCurrent(it.i, it.kind)}
        class:past={it.i < idx}
        class:selected={isSelected(it.i)}
        data-i={it.i}
        title={formatTime(it.p.capturedAt)}
      >
        <img src={api.previewUrl(it.p.id, 256)} alt="" draggable="false" loading="lazy" />
        <Badge decision={it.p.decision} />
      </span>
    {:else}
      <span class="th empty"></span>
    {/if}
  {/each}
</div>

<style>
  .strip { display: flex; gap: 6px; align-items: center; justify-content: center; user-select: none; touch-action: none; }
  .th { position: relative; flex: none; width: var(--ts); height: var(--ts); border-radius: 8px; cursor: pointer; transition: transform 0.12s; }
  .th:hover { transform: translateY(-2px); }
  .th img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; border-radius: inherit; pointer-events: none; }
  .th.cur { width: calc(var(--ts) + 12px); height: calc(var(--ts) + 12px); box-shadow: 0 0 0 2px var(--bg), 0 0 0 4px var(--accent); }
  .th:not(.cur):not(.past) { opacity: 0.8; }
  .selected { outline: 2px solid var(--accent); outline-offset: 2px; }
  .th.empty { background: var(--panel-2); opacity: 0.3; cursor: default; }
  .div { width: 2px; height: calc(var(--ts) * 0.65); background: var(--border); margin: 0 4px; border-radius: 1px; flex: none; }

  /* A burst or near-duplicate group: a tight stack of slices, so you see it coming. */
  .stack { position: relative; flex: none; display: flex; align-items: center; height: var(--ts); padding: 3px 4px; border-radius: 9px; cursor: pointer; background: color-mix(in srgb, var(--pick, #2f6fd0) 30%, transparent); transition: transform 0.12s; }
  .stack:hover { transform: translateY(-2px); }
  .stack:not(.cur):not(.past) { opacity: 0.9; }
  .stack.past { opacity: 0.5; }
  .stack.cur { height: calc(var(--ts) + 12px); box-shadow: 0 0 0 2px var(--bg), 0 0 0 4px var(--accent); }
  .sl { position: relative; flex: none; width: calc(var(--ts) * 0.24); height: calc(var(--ts) - 6px); margin-right: -2px; border-radius: 4px; overflow: hidden; box-shadow: -1.5px 0 0 var(--bg); }
  .sl.last { width: calc(var(--ts) * 0.5); margin-right: 0; }
  .stack.cur .sl { height: calc(var(--ts) + 6px); }
  .sl img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; pointer-events: none; }
  .bn { position: absolute; left: 5px; top: 5px; z-index: 2; font: 600 9px/1 ui-monospace, 'SF Mono', monospace; font-style: normal; background: #000b; color: #fff; border-radius: 4px; padding: 2px 3px; pointer-events: none; }

  /* A set-aside photo: skipped by swiping, but still there to click. */
  .sw { flex: none; width: 7px; height: calc(var(--ts) * 0.8); border-radius: 3px; background: var(--hate); opacity: 0.8; cursor: pointer; }
  .sw:hover { opacity: 1; }
  .sw.cur { width: 9px; height: calc(var(--ts) + 6px); opacity: 1; box-shadow: 0 0 0 2px var(--bg), 0 0 0 4px var(--hate); }
  .sw.done { background: transparent; outline: 1.5px dashed var(--muted); outline-offset: -1.5px; opacity: 0.7; }
</style>
