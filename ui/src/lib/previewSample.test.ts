// Run with `npm run test:ui` (Node's built-in test runner; no extra dependencies).
import assert from 'node:assert/strict';
import { test } from 'node:test';

import { previewGroups, samplePreviews } from './previewSample.ts';

const order = (entries: [string, number][]) => new Map(entries);

test('previewGroups: whole tags first, then runs; newest selection on top', () => {
  const cases: { name: string; days: string[]; order: [string, number][]; tags: { days: string[] }[]; want: string[][] }[] = [
    { name: 'a gap of two days stays one run, three splits', days: ['2026-05-01', '2026-05-03', '2026-05-07'], order: [], tags: [], want: [['2026-05-01', '2026-05-03'], ['2026-05-07']] },
    { name: 'the later selection comes first', days: ['2026-05-01', '2026-06-01'], order: [['2026-05-01', 2], ['2026-06-01', 1]], tags: [], want: [['2026-05-01'], ['2026-06-01']] },
    { name: 'a fully selected tag is one group; a partial tag is ignored', days: ['2026-05-01', '2026-05-09', '2026-05-20'], order: [], tags: [{ days: ['2026-05-01', '2026-05-09'] }, { days: ['2026-05-20', '2026-05-21'] }], want: [['2026-05-01', '2026-05-09'], ['2026-05-20']] },
  ];
  for (const c of cases) assert.deepEqual(previewGroups(c.days, order(c.order), c.tags), c.want, c.name);
});

test('samplePreviews: drops set-aside, one best photo per slice, in capture order', () => {
  const photo = (id: number, score: number | null, setAside = false) => ({ id, day: '2026-05-01', score, setAside });
  const many = Array.from({ length: 30 }, (_, i) => photo(i, i === 4 ? 9 : i === 25 ? 8 : 1));
  const cases = [
    { name: 'nothing', items: [], want: [] },
    { name: 'only set-aside', items: [photo(1, 5, true)], want: [] },
    { name: 'all unscored picks the middle', items: [photo(1, null), photo(2, null), photo(3, null)], want: [2] },
    { name: 'unscored loses to any score', items: [photo(1, null), photo(2, 0.1)], want: [2] },
    { name: 'thirty photos make three slices', items: many, want: [4, 10, 25] },
  ];
  for (const c of cases) assert.deepEqual(samplePreviews(c.items), c.want, c.name);
});
