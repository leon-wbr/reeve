import { test } from 'node:test';
import assert from 'node:assert/strict';
import { loadConfig, isVerbose } from '../src/config.js';

test('defaults', () => {
  assert.equal(loadConfig().verbosity, 'info');
});

test('isVerbose', () => {
  assert.equal(isVerbose(loadConfig({ verbosity: 'debug' })), true);
});
