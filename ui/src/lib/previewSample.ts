// Pure helpers for the Pick screen's preview grid: which selected days sit together, and which photos stand for them.
import { diffDays } from './days.ts';

/** A day with no photos between two shooting days doesn't split a trip: up to this many days apart is one run. */
const MAX_RUN_GAP_DAYS = 2;
const MAX_SAMPLES = 20;
const PHOTOS_PER_SAMPLE = 10;

export type SampleItem = { id: number; day: string; score: number | null; setAside: boolean };

/** Selected days in groups, most recently selected first: whole tags (larger first), then runs of nearby days. */
export function previewGroups(selectedDays: string[], dayOrder: Map<string, number>, tags: { days: string[] }[]): string[][] {
  const selected = new Set(selectedDays);
  const claimed = new Set<string>();
  const groups: string[][] = [];

  const wholeTags = tags
    .filter((t) => t.days.length > 0 && t.days.every((d) => selected.has(d)))
    .sort((a, b) => b.days.length - a.days.length);
  for (const t of wholeTags) {
    if (t.days.some((d) => claimed.has(d))) continue;
    t.days.forEach((d) => claimed.add(d));
    groups.push([...t.days].sort());
  }

  let run: string[] = [];
  for (const day of selectedDays.filter((d) => !claimed.has(d)).sort()) {
    const startsNewRun = run.length > 0 && diffDays(run[run.length - 1], day) > MAX_RUN_GAP_DAYS;
    if (startsNewRun) groups.push(run);
    run = startsNewRun ? [day] : [...run, day];
  }
  if (run.length) groups.push(run);

  const newest = (g: string[]) => Math.max(...g.map((d) => dayOrder.get(d) ?? 0));
  return groups.sort((a, b) => newest(b) - newest(a) || (a[0] < b[0] ? -1 : 1));
}

/** One group's photos (capture order) boiled down to a few: the best-scored photo of each equal slice. */
export function samplePreviews(items: SampleItem[]): number[] {
  const kept = items.filter((p) => !p.setAside);
  if (!kept.length) return [];
  const slices = Math.min(MAX_SAMPLES, Math.max(1, Math.round(kept.length / PHOTOS_PER_SAMPLE)));
  const ids: number[] = [];
  for (let k = 0; k < slices; k++) {
    const slice = kept.slice(Math.floor((k * kept.length) / slices), Math.floor(((k + 1) * kept.length) / slices));
    const scored = slice.filter((p) => p.score !== null);
    const best = scored.length ? scored.reduce((a, b) => (b.score! > a.score! ? b : a)) : slice[Math.floor(slice.length / 2)];
    ids.push(best.id);
  }
  return ids;
}
