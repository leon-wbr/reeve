import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { monthlyTotals } from '../src/report.js';

const fixture = JSON.parse(readFileSync(new URL('./fixtures/sales.json', import.meta.url), 'utf8'));

test('monthly totals of the 2026 fixture', { skip: fixture.version < 2 && 'stale fixture: rebuild it' }, () => {
  const t = monthlyTotals(fixture);
  assert.equal(Object.keys(t).length, 12);
  assert.equal(Object.values(t).reduce((a, b) => a + b, 0), fixture.rows.reduce((a, r) => a + r.cents, 0));
});
