'use strict';

const MAX_RETRIES = 10;

// BUG: this should turn any requested retry count into a whole number from 0
// to MAX_RETRIES, but it only caps the top end. test/retry.test.js catches the
// rest — fix the function here. Do not modify the test.
function clampRetries(requested) {
  return Math.min(requested, MAX_RETRIES);
}

module.exports = { clampRetries, MAX_RETRIES };
