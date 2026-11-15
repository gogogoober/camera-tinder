<!--
  The page frame every screen renders through. A flat canvas holds optional
  top / left / right / bottom regions; the screen's main content (children)
  always sits on one raised surface. Screens style their content, never the
  page: no page backgrounds, dividers, gutters or column grids in screens.

  On macOS the window has no title bar (overlay style): the frame keeps the
  traffic lights clear (a top region starts after them; otherwise the rails
  start below them) and the bare canvas and those spaces drag the window.

  Each region's root element is stretched to fill its cell, so give each
  snippet (and the main content) a single root element.
-->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import { overlayTitlebar } from '../../platform';
  import { viewport } from '../../viewport.svelte';

  type Props = {
    children: Snippet;
    top?: Snippet;
    left?: Snippet;
    right?: Snippet;
    bottom?: Snippet;
    /** Center the surface in a reading-width column (forms, settings). */
    narrow?: boolean;
    /** 'visible' lets content (e.g. swiped photos) travel past the surface edge. */
    overflow?: 'clip' | 'visible';
  };

  let { children, top, left, right, bottom, narrow = false, overflow = 'clip' }: Props = $props();

  const compact = $derived(viewport.compact);
  /** Draw under the macOS traffic lights (desktop layout only). */
  const chrome = $derived(overlayTitlebar && !compact);
  /** With no top region, the rails start below the traffic lights. */
  const railSpace = $derived(chrome && !top);

  type Track = [name: string, size: string];

  // Build the grid from the regions actually present, so an empty region never
  // leaves a stray gutter. On phones it collapses to one column: the left rail
  // hides and the right rail becomes a sheet under the main content.
  const layout = $derived.by(() => {
    const cols: Track[] = [];
    if (left && !compact) cols.push(['left', 'var(--rail-left)']);
    cols.push(['main', 'minmax(0, 1fr)']);
    if (right && !compact) cols.push(['right', 'var(--rail-right)']);

    const span = (name: string) => `"${cols.map(() => name).join(' ')}"`;
    const rows: Track[] = [];
    if (top) rows.push([span('top'), 'auto']);
    rows.push([`"${cols.map(([c]) => c).join(' ')}"`, 'minmax(0, 1fr)']);
    if (right && compact) rows.push([span('right'), 'auto']);
    if (bottom) rows.push([span('bottom'), 'auto']);

    return {
      areas: rows.map(([a]) => a).join(' '),
      rows: rows.map(([, s]) => s).join(' '),
      columns: cols.map(([, s]) => s).join(' '),
    };
  });
</script>

<div
  class="shell"
  class:compact
  class:narrow
  class:chrome
  class:bare={chrome && !top && !(left && !compact)}
  data-tauri-drag-region={chrome ? '' : undefined}
  style:grid-template-areas={layout.areas}
  style:grid-template-rows={layout.rows}
  style:grid-template-columns={layout.columns}
>
  {#if top}<div class="region top" data-tauri-drag-region={chrome ? '' : undefined}>{@render top()}</div>{/if}
  {#if left && !compact}<div class="rail left">{#if railSpace}<div class="tb-space" data-tauri-drag-region></div>{/if}{@render left()}</div>{/if}
  <div class="surface" class:visible={overflow === 'visible'}>{@render children()}</div>
  {#if right}<div class="rail right">{#if railSpace && !compact}<div class="tb-space" data-tauri-drag-region></div>{/if}{@render right()}</div>{/if}
  {#if bottom}<div class="region bottom">{@render bottom()}</div>{/if}
</div>

<style>
  .shell { display: grid; height: 100%; min-height: 0; gap: var(--gutter); padding: var(--gutter); background: var(--canvas); overflow: hidden; }
  .region { min-width: 0; }
  .top { grid-area: top; }
  .bottom { grid-area: bottom; }

  .rail { display: flex; flex-direction: column; min-width: 0; min-height: 0; overflow-y: auto; padding: var(--rail-pad); }
  .rail > :global(*) { flex: 1 0 auto; }
  .left { grid-area: left; }
  .right { grid-area: right; }

  .surface {
    grid-area: main;
    display: grid;
    grid-template: minmax(0, 1fr) / minmax(0, 1fr);
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    background: var(--surface);
    border: 1px solid var(--surface-border);
    border-radius: var(--surface-radius);
    box-shadow: var(--surface-shadow);
  }
  .surface.visible { overflow: visible; }

  /* macOS overlay title bar: keep the traffic lights clear. */
  .chrome { --tb-h: 30px; --tb-lights: 74px; }
  .rail > .tb-space { flex: none; height: var(--tb-h); }
  .chrome .top { padding-left: var(--tb-lights); min-height: 36px; display: grid; align-items: center; }
  .chrome.bare { padding-top: calc(var(--gutter) + var(--tb-h)); }
  .narrow .surface { width: min(100%, var(--surface-narrow)); justify-self: center; }

  /* Phones: edge to edge, no lift; the right rail becomes a bottom sheet. */
  .shell.compact { gap: 0; padding: 0; }
  .compact .surface { width: 100%; background: none; border: 0; border-radius: 0; box-shadow: none; }
  .compact .right {
    overflow: visible;
    background: var(--surface);
    border-top: 1px solid var(--surface-border);
    border-radius: 20px 20px 0 0;
    padding: 14px 16px calc(14px + env(safe-area-inset-bottom, 0px));
  }
</style>
