import { test } from 'node:test';
import assert from 'node:assert/strict';
import { shuffle } from '../src/random.js';

test('shuffle keeps the elements', () => {
  assert.deepEqual(shuffle([1, 2, 3, 4]).sort(), [1, 2, 3, 4]);
});

test('shuffle moves the first element', () => {
  assert.notEqual(shuffle([1, 2, 3, 4])[0], 1);
});
