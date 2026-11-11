import type { DaySummary } from './api';

export const DAY_MS = 86_400_000;
/** Seed pace until the app learns the real one: 15 min ≈ 200 photos. */
export const SECONDS_PER_PHOTO = 4.5;

export const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

export const parseDay = (day: string) => {
  const [y, m, d] = day.split('-').map(Number);
  return new Date(y, m - 1, d);
};

export const dayKey = (d: Date) =>
  `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;

export const addDays = (day: string, n: number) => {
  const d = parseDay(day);
  d.setDate(d.getDate() + n);
  return dayKey(d);
};

/** Whole days from `a` to `b` (DST-safe). */
export const diffDays = (a: string, b: string) => Math.round((parseDay(b).getTime() - parseDay(a).getTime()) / DAY_MS);

export function short(day: string): string {
  const d = parseDay(day);
  return `${MONTHS[d.getMonth()]} ${d.getDate()}`;
}

export function shortRange(a: string, b: string): string {
  return a === b ? short(a) : `${short(a)} – ${short(b)}`;
}

/** Label for a set of days: "Nov 5 – Nov 9" when they're consecutive, else "Aug 1 – Nov 7 · 8 days". */
export function daysLabel(days: string[]): string {
  const ds = [...days].sort();
  if (!ds.length) return '';
  const span = shortRange(ds[0], ds[ds.length - 1]);
  return diffDays(ds[0], ds[ds.length - 1]) + 1 === ds.length ? span : `${span} · ${ds.length} days`;
}

export const minutesFor = (photos: number) => Math.max(1, Math.round((photos * SECONDS_PER_PHOTO) / 60));

/** Heat level of a day: how many photos are still to cull. */
export type Heat = 'none' | 'done' | 'l1' | 'l2' | 'l3' | 'l4';
export function heat(d: DaySummary | undefined): Heat {
  if (!d || d.stats.total === 0) return 'none';
  const left = d.stats.undecided;
  if (left === 0) return 'done';
  return left <= 10 ? 'l1' : left <= 40 ? 'l2' : left <= 100 ? 'l3' : 'l4';
}

/**
 * Pick a day range that fits a time budget. Walks days that still have photos to
 * cull, oldest or newest first, until the budget is used.
 */
export function rangeForBudget(days: DaySummary[], minutes: number, order: 'oldest' | 'newest'): { from: string; to: string } | null {
  const todo = days.filter((d): d is DaySummary & { day: string } => d.day != null && d.stats.undecided > 0);
  if (!todo.length) return null;
  const cap = (minutes * 60) / SECONDS_PER_PHOTO;
  const seq = order === 'oldest' ? todo : [...todo].reverse();
  let n = 0;
  let from = seq[0].day;
  let to = seq[0].day;
  for (const d of seq) {
    if (n > 0 && n + d.stats.undecided > cap * 1.15) break;
    n += d.stats.undecided;
    if (d.day < from) from = d.day;
    if (d.day > to) to = d.day;
  }
  return { from, to };
}

/** Photos taken before this hour count as the previous day (mirrors `cull_core::days`). */
export const DAY_START_HOUR = 6;

/** The day (`YYYY-MM-DD`) a photo belongs to, or null when undated. `capturedAt` is local `YYYY-MM-DDTHH:MM:SS`. */
export function photoDay(capturedAt: string | null | undefined): string | null {
  if (!capturedAt) return null;
  const hour = Number(capturedAt.slice(11, 13));
  const day = capturedAt.slice(0, 10);
  return Number.isFinite(hour) && hour < DAY_START_HOUR ? addDays(day, -1) : day;
}
