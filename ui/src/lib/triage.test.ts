// Run with `npm run test:ui` (Node's built-in test runner; no extra dependencies).
import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { TriageItem } from './api/types.ts';
import { burstStripLayout, drawnItems, firstUndecidedFrame, stripWindow, isStop, matchesChips, nextRescueMark, nextStop, nextUndecidedFrame, sameOrder, setAsideLeft, stepFrame, stopDone, stopOf, structure } from './triage.ts';

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

test('rescue marks cycle keep → favorite → hate → clear', () => {
  assert.equal(nextRescueMark(undefined), 'like');
  assert.equal(nextRescueMark('like'), 'favorite');
  assert.equal(nextRescueMark('favorite'), 'hate');
  assert.equal(nextRescueMark('hate'), undefined);
});

test('an empty chip row shows everything; a selected row narrows', () => {
  assert.equal(matchesChips(new Set<string>(), 'a'), true);
  assert.equal(matchesChips(new Set(['a']), 'a'), true);
  assert.equal(matchesChips(new Set(['a']), 'b'), false);
  assert.equal(matchesChips(new Set(['a']), undefined), false);
});

test('a newer triage is only taken for the very same photos in the same order', () => {
  const ph = [{ id: 1 }, { id: 2 }];
  assert.equal(sameOrder(ph, [{ photoId: 1 }, { photoId: 2 }]), true);
  assert.equal(sameOrder(ph, [{ photoId: 2 }, { photoId: 1 }]), false);
  assert.equal(sameOrder(ph, [{ photoId: 1 }]), false, 'a scan added or removed photos');
});

test('with no triage at all the screen behaves like plain swiping (feature off, or not loaded yet)', () => {
  const none = structure([]);
  const three = [{ decision: null }, { decision: 'like' as const }, { decision: null }];
  assert.deepEqual([0, 1, 2].map((i) => isStop(none, i)), [true, true, true]);
  assert.equal(nextStop(none, three, 0), 2, 'skips the decided one');
  assert.deepEqual(setAsideLeft(none, three), []);
});

test('the filmstrip window counts drawn items, so a long burst is one slot, not twenty', () => {
  // 0 single · 1..20 a 20-frame burst · 21 sweep · 22 single
  const items: TriageItem[] = [judge(0), ...Array.from({ length: 20 }, (_, k) => pick(1 + k, 1, k)), sweep(21), judge(22)];
  const st = structure(items);
  const drawn = drawnItems(st, items.length);
  assert.deepEqual(drawn, [0, 1, 21, 22]);
  assert.deepEqual(stripWindow(st, drawn, 7, 1, 2), [0, 1, 21, 22], 'cursor on frame 7: the stack is centred, neighbours on both sides');
  assert.deepEqual(stripWindow(st, drawn, 0, 2, 1), [undefined, undefined, 0, 1], 'empty slots keep the first item centred');
  assert.deepEqual(stripWindow(st, drawn, items.length, 2, 1), [21, 22, undefined, undefined], 'past the end: last items stay in view');
});

test('burst strip: fits 10 frames, grows 15% per 5 more, then scrolls rather than shrinking to slivers', () => {
  const wide = 2000;
  const ten = burstStripLayout(10, wide);
  assert.equal(ten.frameWidth, 54);
  assert.equal(ten.scrolls, false);
  const fifteen = burstStripLayout(15, wide);
  assert.ok(fifteen.rowWidth > ten.rowWidth && fifteen.rowWidth <= ten.rowWidth * 1.15 + 1, 'grows by one step');
  assert.ok(fifteen.frameWidth < 54 && fifteen.frameWidth >= 28, `${fifteen.frameWidth}`);
  assert.equal(fifteen.scrolls, false);
  assert.equal(burstStripLayout(20, wide).scrolls, false, '20 frames still fit without scrolling');
  const many = burstStripLayout(60, 700);
  assert.equal(many.frameWidth, 28, 'never thinner than the minimum');
  assert.equal(many.scrolls, true);
  assert.ok(many.rowWidth <= 700);
  const narrow = burstStripLayout(4, 200);
  assert.ok(narrow.rowWidth <= 200, 'a phone never overflows the available width');
});
