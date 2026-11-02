import { test } from 'node:test';
import assert from 'node:assert/strict';
import { formatPrice } from '../src/price.js';
import { vat } from '../src/tax.js';
import { needsReorder } from '../src/inventory.js';
import { slugify } from '../src/slug.js';
import { total } from '../src/cart.js';

test('formatPrice', () => {
  assert.equal(formatPrice(1234), '12.34 EUR');
  assert.equal(formatPrice(5, 'USD'), '0.05 USD');
});

test('vat', () => {
  assert.equal(vat(1000, 'DE'), 190);
  assert.equal(vat(999, 'FR'), 200);
  assert.throws(() => vat(1000, 'XX'));
});

test('needsReorder', () => {
  assert.equal(needsReorder({ stock: 2 }), true);
  assert.equal(needsReorder({ stock: 9 }), false);
});

test('slugify', () => {
  assert.equal(slugify('Blue Mug, large'), 'blue-mug-large');
  assert.equal(slugify('Crème Brûlée'), 'creme-brulee');
});

test('cart total', () => {
  assert.equal(total([{ cents: 1000, country: 'DE' }, { cents: 500, country: 'FR' }]), 1790);
});
