import assert from 'node:assert/strict';
import { test } from 'node:test';
import { photoDay, suggestionsInView } from './days.ts';

test('a day starts at 6 a.m.', () => {
  assert.equal(photoDay('2026-10-04T01:30:00'), '2026-10-03');
  assert.equal(photoDay('2026-10-04T05:59:59'), '2026-10-03');
  assert.equal(photoDay('2026-10-04T06:00:00'), '2026-10-04');
  assert.equal(photoDay('2027-01-01T02:00:00'), '2026-12-31');
  assert.equal(photoDay('2026-03-01T03:00:00'), '2026-02-28');
  assert.equal(photoDay(null), null);
});

type Sug = { name: string; startDay: string; endDay: string; children: Sug[] };
const sug = (name: string, startDay: string, endDay: string, children: Sug[] = []): Sug => ({ name, startDay, endDay, children });

test('suggestions follow the visible months', () => {
  const october = sug('october', '2026-10-05', '2026-10-09');
  const spring = sug('spring', '2026-03-01', '2026-03-04');
  const trip = sug('trip', '2026-03-01', '2026-10-31', [sug('early', '2026-03-01', '2026-03-02'), sug('late', '2026-10-20', '2026-10-22')]);
  const visible = new Set(['2026-10']);
  const cases = [
    { name: 'no month filter shows everything', list: [october, spring], months: null, keep: () => false, names: ['october', 'spring'] },
    { name: 'overlapping a visible month shows', list: [october], months: visible, keep: () => false, names: ['october'] },
    { name: 'outside every visible month is hidden', list: [spring], months: visible, keep: () => false, names: [] },
    { name: 'outside but kept stays', list: [spring], months: visible, keep: (s: Sug) => s.name === 'spring', names: ['spring'] },
  ];
  for (const c of cases) {
    assert.deepEqual(suggestionsInView(c.list, c.months, c.keep).map((s) => s.name), c.names, c.name);
  }
  const shown = suggestionsInView([sug('parent', '2026-03-01', '2026-03-04', trip.children)], visible, () => false);
  assert.deepEqual(shown.map((s) => [s.name, s.children.map((c) => c.name)]), [['parent', ['late']]]);
});
