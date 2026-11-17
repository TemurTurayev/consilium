'use strict';

// Protected oracle: restored from the baseline commit before scoring.
const test = require('node:test');
const assert = require('node:assert/strict');

const api = require('../user.js');

const ada = { name: 'Ada Lovelace', email: 'ada@example.com' };

// Resolves with the next DeprecationWarning the process emits.
function nextDeprecationWarning() {
  return new Promise((resolve) => {
    const onWarning = (warning) => {
      if (warning.name === 'DeprecationWarning') {
        process.off('warning', onWarning);
        resolve(warning);
      }
    };
    process.on('warning', onWarning);
  });
}

test('formatUser is the public name', () => {
  assert.equal(typeof api.formatUser, 'function');
  assert.equal(api.formatUser(ada), 'Ada Lovelace <ada@example.com>');
});

test('formatUser itself does not warn', { timeout: 2000 }, async () => {
  let warned = false;
  const onWarning = () => {
    warned = true;
  };
  process.on('warning', onWarning);
  api.formatUser(ada);
  await new Promise((resolve) => setTimeout(resolve, 50));
  process.off('warning', onWarning);
  assert.equal(warned, false);
});

test('format_user still works and warns that it is deprecated', { timeout: 2000 }, async () => {
  assert.equal(typeof api.format_user, 'function');
  const warning = nextDeprecationWarning();
  assert.equal(api.format_user(ada), 'Ada Lovelace <ada@example.com>');
  const { message } = await warning;
  assert.match(message, /format_user/);
  assert.match(message, /formatUser/);
});
