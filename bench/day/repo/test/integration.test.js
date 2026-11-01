// Integration suite: runs against the shared test database, which takes about a minute.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { config, withDatabase } from '../scripts/db.mjs';
import { total } from '../src/cart.js';
import { formatPrice } from '../src/price.js';

test('checkout against the test database', async () => {
  await withDatabase('suite', config.holdSeconds, () => {
    assert.equal(formatPrice(total([{ cents: 2000, country: 'DE' }])), '23.80 EUR');
  });
});
