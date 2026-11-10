<script lang="ts">
  /**
   * Empty-state splash for the Pick screen: a line-drawn viewfinder whose scene drifts into focus,
   * then the focus box turns orange and a check mark draws in. Loops slowly; static when the user
   * prefers reduced motion.
   */
  let { onAdd, busy = false }: { onAdd: () => void; busy?: boolean } = $props();
</script>

<div class="splash">
  <svg viewBox="0 0 240 160" fill="none" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <!-- Viewfinder corners -->
    <path class="frame" d="M20 36V16h20M200 16h20v20M220 124v20h-20M40 144H20v-20" />
    <!-- The scene: hills, sun, horizon. Blurred, then sharp. -->
    <g class="scene">
      <path d="M34 120l40-46 26 26 34-44 70 64" />
      <circle cx="172" cy="50" r="11" />
      <path d="M34 120h172" />
      <path d="M58 132h48M128 132h56" class="faint" />
    </g>
    <!-- Focus box, then the check once it's sharp. -->
    <rect class="box" x="104" y="64" width="32" height="32" rx="3" />
    <path class="check" d="M112 80l6 6 11-12" />
    <text x="28" y="156" class="exif">f/2.8 · 1/250 · ISO 100</text>
  </svg>

  <h2>Darkroom</h2>
  <p>Triage your shoots. Pick the days, swipe through the photos, keep the best.</p>
  <button class="primary" onclick={onAdd} disabled={busy}>+ Add photos</button>
</div>

<style>
  .splash { display: flex; flex-direction: column; align-items: center; text-align: center; gap: 10px; margin: auto; padding: 40px 16px; max-width: 460px; }
  svg { width: min(360px, 100%); height: auto; margin-bottom: 8px; }
  h2 { margin: 0; font-size: 26px; letter-spacing: -0.01em; }
  p { margin: 0 0 10px; color: var(--muted); line-height: 1.45; }
  button { padding: 10px 18px; font-size: 15px; }

  .frame { stroke: var(--text); stroke-width: 1.6; }
  .scene { stroke: var(--text); stroke-width: 1.4; animation: focus 5s ease-in-out infinite; }
  .faint { opacity: 0.45; }
  .box { stroke: var(--muted); stroke-width: 1.4; animation: box 5s ease-in-out infinite; }
  .check { stroke: var(--accent); stroke-width: 2.4; stroke-dasharray: 30; stroke-dashoffset: 30; animation: check 5s ease-in-out infinite; }
  .exif { font: 500 9px ui-monospace, 'SF Mono', monospace; fill: var(--muted); stroke: none; }

  /* 0–35% soft, 45% sharp, check draws 50–60%, holds, fades back for the next loop. */
  @keyframes focus {
    0%, 30% { filter: blur(2.2px); opacity: 0.5; }
    45%, 88% { filter: blur(0); opacity: 1; }
    100% { filter: blur(2.2px); opacity: 0.5; }
  }
  @keyframes box {
    0%, 42% { stroke: var(--muted); stroke-dasharray: 4 4; }
    46%, 88% { stroke: var(--accent); stroke-dasharray: none; }
    100% { stroke: var(--muted); stroke-dasharray: 4 4; }
  }
  @keyframes check {
    0%, 48% { stroke-dashoffset: 30; opacity: 1; }
    60%, 86% { stroke-dashoffset: 0; opacity: 1; }
    94%, 100% { stroke-dashoffset: 0; opacity: 0; }
  }

  @media (prefers-reduced-motion: reduce) {
    .scene, .box, .check { animation: none; }
    .box { stroke: var(--accent); }
    .check { stroke-dashoffset: 0; }
  }
</style>
