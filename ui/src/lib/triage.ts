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
  const g = s.groupAt[i];
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

/** The previous (-1) or next (+1) member of the group, without wrapping; null at the ends. */
export function stepFrame(s: Structure, i: number, dir: 1 | -1): number | null {
  const g = groupOf(s, i);
  return g[g.indexOf(i) + dir] ?? null;
}

/** Set-aside photos nobody has decided yet. */
export function setAsideLeft(s: Structure, photos: Decided[]): number[] {
  return photos.map((_, i) => i).filter((i) => s.setAside[i] && !photos[i].decision);
}
