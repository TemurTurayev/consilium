'use strict';

// Protected oracle: restored from the baseline commit before scoring.
const test = require('node:test');
const assert = require('node:assert/strict');

const { clampRetries, MAX_RETRIES } = require('../retry.js');

test('keeps whole numbers inside the range', () => {
  assert.equal(clampRetries(0), 0);
  assert.equal(clampRetries(3), 3);
  assert.equal(clampRetries(10), 10);
});

test('caps values above the maximum', () => {
  assert.equal(MAX_RETRIES, 10);
  assert.equal(clampRetries(11), 10);
  assert.equal(clampRetries(250), 10);
  assert.equal(clampRetries(Infinity), 10);
});

test('raises negative values to zero', () => {
  assert.equal(clampRetries(-1), 0);
  assert.equal(clampRetries(-99), 0);
  assert.equal(clampRetries(-Infinity), 0);
});

test('drops the fractional part', () => {
  assert.equal(clampRetries(2.9), 2);
  assert.equal(clampRetries(9.99), 9);
  assert.equal(clampRetries(10.5), 10);
});

test('treats NaN as zero retries', () => {
  assert.equal(clampRetries(NaN), 0);
});
