export function formatDay(day: string | null): string {
  if (!day) return 'Unknown date';
  const d = new Date(`${day}T00:00:00`);
  return d.toLocaleDateString(undefined, { weekday: 'short', month: 'short', day: 'numeric' });
}

export function formatTime(capturedAt: string | null): string {
  if (!capturedAt) return '';
  const d = new Date(capturedAt);
  return d.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit' });
}

export function formatCount(n: number): string {
  return n.toLocaleString();
}

/** Preview size matched to the screen, so we never decode more than we show. */
export function previewEdge(): number {
  const px = Math.max(window.screen.width, window.screen.height) * (window.devicePixelRatio || 1);
  return Math.min(2560, Math.max(1024, Math.round(px / 256) * 256));
}
