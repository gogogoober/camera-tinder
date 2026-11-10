// Run with `npm run test:ui` (Node's built-in test runner; no extra dependencies).
import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { TriageItem } from './api/types.ts';
import { firstUndecidedFrame, isStop, nextStop, nextUndecidedFrame, setAsideLeft, stepFrame, stopDone, stopOf, structure } from './triage.ts';

const judge = (photoId: number): TriageItem => ({ photoId, question: 'judge', group: null, reasons: [], hints: [], score: null, analyzed: true });
const sweep = (photoId: number): TriageItem => ({ ...judge(photoId), question: 'sweep', reasons: ['blurry'] });
const pick = (photoId: number, groupId: number, rank: number): TriageItem => ({
  ...judge(photoId),
  question: 'pick',
  group: { id: groupId, kind: 'burst', size: 3, rank, suggested: rank === 0 },
});

// 0 single · 1 sweep · 2,3,4 burst (group 2) · 5 single · 6 sweep
const triage = [judge(0), sweep(1), pick(2, 2, 1), pick(3, 2, 0), pick(4, 2, 2), judge(5), sweep(6)];
const s = structure(triage);
const photos = (decided: number[] = []) => triage.map((_, i) => ({ decision: decided.includes(i) ? ('like' as const) : null }));

test('stops are singles and the first member of each group, never set-aside photos', () => {
  assert.deepEqual([...triage.keys()].filter((i) => isStop(s, i)), [0, 2, 5]);
  assert.equal(stopOf(s, 4), 2);
  assert.equal(stopOf(s, 1), 1);
});

test('a group is done only when every member is decided', () => {
  assert.equal(stopDone(s, photos([2, 3]), 2), false);
  assert.equal(stopDone(s, photos([2, 3, 4]), 4), true);
});

test('nextStop skips set-aside photos and finished stops, wraps once, and ends with -1', () => {
  assert.equal(nextStop(s, photos(), 0), 2, 'skips the sweep at 1');
  assert.equal(nextStop(s, photos(), 2), 5);
  assert.equal(nextStop(s, photos(), 5), 0, 'wraps');
  assert.equal(nextStop(s, photos([0, 2, 3, 4]), 0), 5);
  assert.equal(nextStop(s, photos([0, 2, 3, 4, 5]), 0), -1, 'set-aside photos alone never keep the cursor going');
});

test('frames: first undecided, next undecided inside the group, and stepping with [ ]', () => {
  assert.equal(firstUndecidedFrame(s, photos([2]), 2), 3);
  assert.equal(firstUndecidedFrame(s, photos([2, 3, 4]), 2), 2, 'all decided: the first');
  assert.equal(nextUndecidedFrame(s, photos([2]), 3), 4);
  assert.equal(nextUndecidedFrame(s, photos([2]), 4), 3, 'wraps inside the group');
  assert.equal(nextUndecidedFrame(s, photos([2, 3, 4]), 4), null);
  assert.equal(stepFrame(s, 2, -1), null);
  assert.equal(stepFrame(s, 2, 1), 3);
  assert.equal(stepFrame(s, 4, 1), null);
});

test('set-aside photos left for the end', () => {
  assert.deepEqual(setAsideLeft(s, photos()), [1, 6]);
  assert.deepEqual(setAsideLeft(s, photos([1])), [6]);
});
