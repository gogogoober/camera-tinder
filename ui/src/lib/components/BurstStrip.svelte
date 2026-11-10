<script lang="ts">
  import { api, type Photo, type TriageItem } from '../api';
  import { burstStripLayout } from '../triage';
  import Badge from './Badge.svelte';

  // The small second timeline under the photo while the cursor is on a burst or near-duplicate group:
  // one thumbnail per frame. Nothing is pre-selected; ★ is only a suggestion.
  let {
    photos,
    triage,
    members,
    frame,
    compact = false,
    onPick,
  }: {
    photos: Photo[];
    triage: TriageItem[];
    /** Photo indexes of the group, in capture order. */
    members: number[];
    /** The photo shown now. */
    frame: number;
    compact?: boolean;
    onPick: (photoIndex: number) => void;
  } = $props();

  const decided = $derived(members.filter((m) => photos[m].decision).length);

  // Frames fit without scrolling up to a point (see `burstStripLayout`), then the row scrolls and
  // follows the selected frame.
  let available = $state(0);
  const layout = $derived(burstStripLayout(members.length, available || 600));
  let row: HTMLDivElement | undefined = $state();
  $effect(() => {
    void frame;
    row?.querySelector('.on')?.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  });
</script>

<div class="burst" class:compact bind:clientWidth={available}>
  <div class="row" bind:this={row} style:width="{layout.rowWidth}px" style:--fw="{layout.frameWidth}px" role="group" aria-label="Frames of this burst">
    {#each members as m, k (m)}
      <button class="fr" class:on={m === frame} class:bad={triage[m]?.reasons.length > 0} onclick={() => onPick(m)} aria-label="Frame {k + 1} of {members.length}">
        <img src={api.previewUrl(photos[m].id, 256)} alt="" draggable="false" />
        {#if triage[m]?.group?.suggested}<i class="star">★</i>{/if}
        {#if triage[m]?.reasons.length}<i class="underline"></i>{/if}
        <Badge decision={photos[m].decision} />
      </button>
    {/each}
  </div>
  <div class="legend muted">
    {decided} of {members.length} decided · {compact ? 'tap' : 'click, or [ ]'} to change frame · ★ is a suggestion
  </div>
</div>

<style>
  .burst { display: grid; gap: 6px; justify-items: center; width: 100%; }
  .row { display: flex; gap: 6px; padding: 8px; box-sizing: border-box; border-radius: 10px; max-width: 100%; overflow-x: auto; background: color-mix(in srgb, var(--pick, #2f6fd0) 10%, var(--panel-2)); border: 1px solid color-mix(in srgb, var(--pick, #2f6fd0) 35%, var(--border)); }
  .fr { position: relative; flex: none; width: var(--fw, 54px); height: calc(var(--fw, 54px) * 0.74); padding: 0; border: 0; border-radius: 6px; background: none; opacity: 0.7; cursor: pointer; }
  .fr img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; border-radius: inherit; pointer-events: none; }
  .fr.on { opacity: 1; outline: 2px solid var(--text); outline-offset: 2px; }
  .star { position: absolute; right: 3px; top: 1px; font-size: 11px; font-style: normal; color: #fff; text-shadow: 0 0 3px #000; }
  .underline { position: absolute; left: 0; right: 0; bottom: 0; height: 3px; border-radius: 0 0 6px 6px; background: var(--hate); }
  .legend { font-size: 12px; text-align: center; }
</style>
