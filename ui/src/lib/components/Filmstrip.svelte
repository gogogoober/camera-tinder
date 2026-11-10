<script lang="ts">
  import { api, type Photo } from '../api';
  import { formatTime } from '../format';
  import { dragSelect } from '../select';
  import Badge from './Badge.svelte';

  let {
    photos,
    idx,
    selection,
    momentOf,
    burstSizeAt,
    back,
    forward,
    size,
    onSelect,
    onJump,
  }: {
    photos: Photo[];
    idx: number;
    selection: { from: number; to: number } | null;
    momentOf: number[];
    /** Size of the burst that starts at this photo index, else 0. */
    burstSizeAt: Map<number, number>;
    back: number;
    forward: number;
    size: number;
    onSelect: (from: number, to: number) => void;
    onJump: (photoIndex: number) => void;
  } = $props();

  const items = $derived(
    Array.from({ length: back + forward + 1 }, (_, k) => {
      const i = idx - back + k;
      const p = photos[i];
      if (!p) return { i, p: null, divider: false };
      return { i, p, divider: k > 0 && photos[i - 1] != null && momentOf[i - 1] !== momentOf[i] };
    }),
  );
</script>

<div
  class="strip"
  style:--ts="{size}px"
  role="group"
  aria-label="Filmstrip"
  use:dragSelect={{ onSelect, onClick: (i) => onJump(i) }}
>
  {#each items as it (it.i)}
    {#if it.divider}<span class="div"></span>{/if}
    {#if it.p}
      <span
        class="th"
        class:cur={it.i === idx}
        class:past={it.i < idx}
        class:selected={!!selection && it.i >= selection.from && it.i <= selection.to}
        data-i={it.i}
        title={formatTime(it.p.capturedAt)}
      >
        <img src={api.previewUrl(it.p.id, 256)} alt="" draggable="false" loading="lazy" />
        {#if burstSizeAt.get(it.i)}<i class="bn">×{burstSizeAt.get(it.i)}</i>{/if}
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
  .th.selected { outline: 2px solid var(--accent); outline-offset: 2px; }
  .th.empty { background: var(--panel-2); opacity: 0.3; cursor: default; }
  .div { width: 2px; height: calc(var(--ts) * 0.65); background: var(--border); margin: 0 4px; border-radius: 1px; flex: none; }
  .bn { position: absolute; left: 3px; top: 3px; font: 600 9px/1 ui-monospace, 'SF Mono', monospace; font-style: normal; background: #000b; color: #fff; border-radius: 4px; padding: 2px 3px; }
</style>
