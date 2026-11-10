// Pure helpers for navigating a triage plan (see docs/smart-triage.md). No Svelte, no API calls:
// the Cull screen asks these where to go next, which keeps the rules in one place and testable
// (`npm run test:ui`).
//
// Vocabulary: a *stop* is one thing the cursor sits on: a single photo, or a whole group (a burst or
// near-duplicates, shown with its frames). A *set-aside* photo (question "sweep") is never a stop:
// swiping skips it; it waits for the Rescue screen, or the user opens it from the filmstrip.

import type { IssueKind } from './api/analysisTypes';
import type { Photo, TriageItem } from './api/types';

type Decided = Pick<Photo, 'decision'>;

export const ISSUE_LABELS: Record<IssueKind, string> = {
  blurry: 'blurry',
  motionBlur: 'motion blur',
  subjectOutOfFocus: 'subject out of focus',
  underexposed: 'too dark',
  overexposed: 'too bright',
  flat: 'flat',
  noisy: 'noisy',
  colorCast: 'colour cast',
  eyesClosed: 'eyes closed',
  faceCutOff: 'face cut off',
  poorFaceQuality: 'poor face quality',
  tilted: 'tilted',
  lowAesthetic: 'low aesthetic score',
  utility: 'screenshot / document',
  accidental: 'accidental shot',
};

/** What the plan's shape says, independent of decisions. Build once per plan. */
export interface Structure {
  /** Group id → member photo indexes, ascending (capture order). */
  members: Map<number, number[]>;
  /** Per photo: its group id, or null. */
  groupAt: (number | null)[];
  setAside: boolean[];
}

export function structure(triage: TriageItem[]): Structure {
  const members = new Map<number, number[]>();
  const groupAt = triage.map((t, i) => {
    if (!t.group) return null;
    members.set(t.group.id, [...(members.get(t.group.id) ?? []), i]);
    return t.group.id;
  });
  return { members, groupAt, setAside: triage.map((t) => t.question === 'sweep') };
}

/** The photos of the group that photo `i` belongs to (just `[i]` when it is on its own). */
export function groupOf(s: Structure, i: number): number[] {
  const g = s.groupAt[i] ?? null; // no triage for this photo (feature off, or plan not loaded): it's a single
  return g === null ? [i] : (s.members.get(g) ?? [i]);
}

/** The index the cursor sits on for photo `i`: its group's first member, else itself. */
export function stopOf(s: Structure, i: number): number {
  return groupOf(s, i)[0];
}

/** Can the cursor sit on photo `i`? Not set-aside photos, and only a group's first member. */
export function isStop(s: Structure, i: number): boolean {
  return !s.setAside[i] && stopOf(s, i) === i;
}

/** A single is done when decided; a group when every member is. */
export function stopDone(s: Structure, photos: Decided[], i: number): boolean {
  return groupOf(s, i).every((m) => !!photos[m].decision);
}

/** The next stop after `from` that still has something to decide, wrapping once; -1 when none is left. */
export function nextStop(s: Structure, photos: Decided[], from: number): number {
  const n = photos.length;
  for (let k = 1; k <= n; k++) {
    const j = (from + k) % n;
    if (isStop(s, j) && !stopDone(s, photos, j)) return j;
  }
  return -1;
}

/** Which frame to show when the cursor lands on a group: its first undecided member. */
export function firstUndecidedFrame(s: Structure, photos: Decided[], i: number): number {
  const g = groupOf(s, i);
  return g.find((m) => !photos[m].decision) ?? g[0];
}

/** The next undecided member of `i`'s group after `i`, wrapping inside the group; null when none is left. */
export function nextUndecidedFrame(s: Structure, photos: Decided[], i: number): number | null {
  const g = groupOf(s, i);
  const at = g.indexOf(i);
  for (let k = 1; k < g.length; k++) {
    const m = g[(at + k) % g.length];
    if (!photos[m].decision) return m;
  }
  return null;
}

/** The previous (-1) or next (+1) member of the group; null at the ends, or when it is a single. With `wrap`, the ends loop. */
export function stepFrame(s: Structure, i: number, dir: 1 | -1, wrap = false): number | null {
  const g = groupOf(s, i);
  const at = g.indexOf(i);
  if (!wrap) return g[at + dir] ?? null;
  return g.length > 1 ? g[(at + dir + g.length) % g.length] : null;
}

/**
 * Space ("next"): always loops inside the context it is in, and never leaves it unrated.
 * In a group: the next frame, wrapping, decided frames included (to compare against a pick).
 * On a single: the next stop that still has something to decide, wrapping to the start of the list.
 * Returns the photo index to show, or null when there is nowhere to go.
 */
export function loopNext(s: Structure, photos: Decided[], i: number): number | null {
  const g = groupOf(s, i);
  if (g.length > 1) return stepFrame(s, i, 1, true);
  const stop = nextStop(s, photos, i);
  return stop < 0 ? null : firstUndecidedFrame(s, photos, stop);
}

/**
 * B ("back"): the mirror of Space, nothing is changed. In a group: the previous frame, wrapping. On a
 * single: the stop before it (decided ones included, so you can look again), wrapping to the end.
 * Returns the photo index to show, or null when there is nowhere to go.
 */
export function loopPrev(s: Structure, i: number): number | null {
  const g = groupOf(s, i);
  if (g.length > 1) return stepFrame(s, i, -1, true);
  const n = s.setAside.length;
  for (let k = 1; k < n; k++) {
    const j = (i - k + n) % n;
    if (isStop(s, j)) return j;
  }
  return null;
}

/**
 * Enter / S ("leave"): out of the current group (or off this single) to the next stop that still has
 * something to decide, leaving what is here unrated. Wraps to the start of the list; the group it left
 * comes round again. null when nothing is left to decide.
 */
export function leaveNext(s: Structure, photos: Decided[], i: number): number | null {
  const stop = nextStop(s, photos, groupOf(s, i).at(-1)!);
  return stop < 0 ? null : firstUndecidedFrame(s, photos, stop);
}

/** Set-aside photos nobody has decided yet. */
export function setAsideLeft(s: Structure, photos: Decided[]): number[] {
  return photos.map((_, i) => i).filter((i) => s.setAside[i] && !photos[i].decision);
}

// ---- Rescue ------------------------------------------------------------------------------

/** What a click on a Rescue photo cycles through: ✓ Keep → ★ Favorite → ⊘ Hate → clear. */
export type RescueMark = 'like' | 'favorite' | 'hate';
const RESCUE_CYCLE: (RescueMark | undefined)[] = [undefined, 'like', 'favorite', 'hate'];

export function nextRescueMark(mark: RescueMark | undefined): RescueMark | undefined {
  return RESCUE_CYCLE[(RESCUE_CYCLE.indexOf(mark) + 1) % RESCUE_CYCLE.length];
}

/** A chip row: nothing selected means no filter (everything shows); otherwise a photo must match one. */
export function matchesChips<T>(selected: Set<T>, value: T | undefined): boolean {
  return selected.size === 0 || (value !== undefined && selected.has(value));
}

/** Is `triage` the plan for exactly these photos, in this order? (Guards swapping in a newer triage.) */
export function sameOrder(photos: Pick<Photo, 'id'>[], triage: Pick<TriageItem, 'photoId'>[]): boolean {
  return photos.length === triage.length && photos.every((p, i) => triage[i].photoId === p.id);
}

// ---- Layout ------------------------------------------------------------------------------

/** The photos the filmstrip draws, in order: singles, set-aside slivers, and each group once (its first member). */
export function drawnItems(s: Structure, count: number): number[] {
  return Array.from({ length: count }, (_, i) => i).filter((i) => s.setAside[i] || stopOf(s, i) === i);
}

/**
 * The filmstrip window: `back` drawn items before the shown one and `forward` after, counted in drawn
 * items (a 20-frame burst is one item). `undefined` = an empty slot, so the shown item stays centred.
 * `shown` past the end (everything done) keeps the last items in view.
 */
export function stripWindow(s: Structure, drawn: number[], shown: number, back: number, forward: number): (number | undefined)[] {
  const anchor = s.setAside[shown] ? shown : stopOf(s, shown);
  const at = drawn.indexOf(anchor);
  const pos = at >= 0 ? at : drawn.length;
  return Array.from({ length: back + forward + 1 }, (_, k) => drawn[pos - back + k]);
}

/** Burst frame strip sizing (px). Up to `FIT` frames fit at full size; every `STEP` frames more add `GROW`. */
/** PAD = the row's padding (8 + 8) and border (1 + 1) in BurstStrip.svelte. */
export const BURST_STRIP = { FRAME_MAX: 54, FRAME_MIN: 28, GAP: 6, PAD: 18, FIT: 10, STEP: 5, GROW: 0.15 } as const;

/**
 * How wide the burst strip and each frame are, given `frames` and the `available` width.
 * The strip starts as wide as `FIT` full-size frames; every `STEP` frames over `FIT` widen it by `GROW`
 * (never past `available`). Frames share that width, between `FRAME_MIN` and `FRAME_MAX`; if even
 * `FRAME_MIN` doesn't fit, the row scrolls instead of shrinking frames into slivers.
 */
export function burstStripLayout(frames: number, available: number): { frameWidth: number; rowWidth: number; scrolls: boolean } {
  const { FRAME_MAX, FRAME_MIN, GAP, PAD, FIT, STEP, GROW } = BURST_STRIP;
  const n = Math.max(1, frames);
  const extent = (frame: number) => n * frame + (n - 1) * GAP + PAD;
  const base = Math.min(available, FIT * FRAME_MAX + (FIT - 1) * GAP + PAD); // FIT full-size frames
  const steps = Math.ceil(Math.max(0, n - FIT) / STEP);
  const allowed = Math.min(available, base * (1 + GROW * steps));
  const frameWidth = Math.floor(Math.min(FRAME_MAX, Math.max(FRAME_MIN, (allowed - PAD - (n - 1) * GAP) / n)));
  const needed = extent(frameWidth);
  return { frameWidth, rowWidth: Math.min(needed, allowed), scrolls: needed > allowed };
}
