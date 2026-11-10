/**
 * Drag-to-select for any row or grid of items marked `data-i="<index>"`.
 *
 * Press on an item and drag across others to select the span between them; a press
 * without movement is a plain click. Works with mouse, pen and touch.
 */
export interface SelectOptions {
  /** Called as the selection grows while dragging, and once more on release. */
  onSelect: (from: number, to: number) => void;
  /** Called for a press-and-release on a single item. */
  onClick?: (index: number, e: PointerEvent) => void;
  /** Called while the pointer is down, for live highlighting. */
  onPreview?: (range: { from: number; to: number } | null) => void;
}

export function dragSelect(node: HTMLElement, opts: SelectOptions) {
  let o = opts;
  let anchor: number | null = null;
  let last: { from: number; to: number } | null = null;
  let moved = false;

  const indexAt = (x: number, y: number): number | null => {
    const el = document.elementFromPoint(x, y)?.closest<HTMLElement>('[data-i]');
    if (!el || !node.contains(el)) return null;
    const i = Number(el.dataset.i);
    return Number.isFinite(i) ? i : null;
  };

  function down(e: PointerEvent) {
    if (e.button !== 0) return;
    const i = indexAt(e.clientX, e.clientY);
    if (i == null) return;
    anchor = i;
    last = { from: i, to: i };
    moved = false;
    node.setPointerCapture(e.pointerId);
  }

  function move(e: PointerEvent) {
    if (anchor == null) return;
    const i = indexAt(e.clientX, e.clientY);
    if (i == null) return;
    const next = { from: Math.min(anchor, i), to: Math.max(anchor, i) };
    if (next.from !== last!.from || next.to !== last!.to) {
      last = next;
      if (next.from !== next.to) moved = true;
      if (moved) {
        o.onPreview?.(next);
        o.onSelect(next.from, next.to);
      }
    }
  }

  function up(e: PointerEvent) {
    if (anchor == null) return;
    const at = anchor;
    anchor = null;
    o.onPreview?.(null);
    if (!moved) o.onClick?.(at, e);
  }

  node.addEventListener('pointerdown', down);
  node.addEventListener('pointermove', move);
  node.addEventListener('pointerup', up);
  node.addEventListener('pointercancel', up);
  node.style.touchAction = 'none';

  return {
    update(next: SelectOptions) {
      o = next;
    },
    destroy() {
      node.removeEventListener('pointerdown', down);
      node.removeEventListener('pointermove', move);
      node.removeEventListener('pointerup', up);
      node.removeEventListener('pointercancel', up);
    },
  };
}
