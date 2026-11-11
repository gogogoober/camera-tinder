import assert from 'node:assert/strict';
import { test } from 'node:test';
import { photoDay } from './days.ts';

test('a day starts at 6 a.m.', () => {
  assert.equal(photoDay('2026-10-04T01:30:00'), '2026-10-03');
  assert.equal(photoDay('2026-10-04T05:59:59'), '2026-10-03');
  assert.equal(photoDay('2026-10-04T06:00:00'), '2026-10-04');
  assert.equal(photoDay('2027-01-01T02:00:00'), '2026-12-31');
  assert.equal(photoDay('2026-03-01T03:00:00'), '2026-02-28');
  assert.equal(photoDay(null), null);
});
