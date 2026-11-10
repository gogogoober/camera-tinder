<script lang="ts">
  import { api, type Decision, type Photo } from '../api';
  import { DECISIONS, decisionForDrag } from '../decisions';

  let {
    photo,
    edge,
    onDecide,
  }: {
    photo: Photo;
    edge: number;
    onDecide: (d: Decision, from: { x: number; y: number }) => void;
  } = $props();

  const THRESHOLD = 110;

  let dx = $state(0);
  let dy = $state(0);
  let dragging = $state(false);
  let start = { x: 0, y: 0 };

  const intent = $derived(decisionForDrag(dx, dy));
  const strength = $derived(Math.min(1, Math.max(Math.abs(dx), Math.abs(dy)) / THRESHOLD));

  function down(e: PointerEvent) {
    if (e.button !== 0) return;
    dragging = true;
    start = { x: e.clientX, y: e.clientY };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }

  function move(e: PointerEvent) {
    if (!dragging) return;
    dx = e.clientX - start.x;
    dy = e.clientY - start.y;
  }

  function up() {
    if (!dragging) return;
    dragging = false;
    if (intent && strength >= 1) onDecide(intent, { x: dx, y: dy });
    dx = 0;
    dy = 0;
  }
</script>

<div
  class="card"
  class:dragging
  style:transform="translate({dx}px, {dy}px) rotate({dx / 22}deg)"
  onpointerdown={down}
  onpointermove={move}
  onpointerup={up}
  onpointercancel={up}
  role="img"
  aria-label={photo.fileName}
>
  <img class="photo-fill" src={api.previewUrl(photo.id, edge)} alt={photo.fileName} draggable="false" />
  {#if intent}
    <div class="stamp {intent}" style:opacity={strength}>{DECISIONS[intent].stamp}</div>
  {/if}
</div>

<style>
  .card {
    position: absolute;
    inset: 0;
    touch-action: none;
    cursor: grab;
    transition: transform 0.2s ease-out;
  }
  .card.dragging { transition: none; cursor: grabbing; }
  /* Fill the stage exactly and letterbox with object-fit. Percentage max-height
     inside grid/flex items is unreliable in WebKit, which let tall (portrait)
     photos overflow and shift. drop-shadow follows the visible photo only. */
  :global(.photo-fill) {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: contain;
    filter: drop-shadow(0 10px 30px #0009);
    pointer-events: none;
  }
  :global(.stamp) {
    position: absolute;
    top: 8%;
    left: 50%;
    translate: -50% 0;
    font-size: 40px;
    font-weight: 800;
    letter-spacing: 0.06em;
    padding: 4px 16px;
    border: 5px solid currentColor;
    border-radius: 12px;
    background: #0006;
    pointer-events: none;
  }
  :global(.stamp.like) { color: var(--like); rotate: -12deg; }
  :global(.stamp.dislike) { color: var(--dislike); rotate: 12deg; }
  :global(.stamp.favorite) { color: var(--favorite); }
  :global(.stamp.hate) { color: var(--hate); }
</style>
