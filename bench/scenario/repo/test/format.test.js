import { test } from 'node:test';
import assert from 'node:assert/strict';
import { formatNumber } from '../src/format.js';

test('formatNumber groups thousands', () => {
  assert.equal(formatNumber(1234567), '1,234,567');
});
