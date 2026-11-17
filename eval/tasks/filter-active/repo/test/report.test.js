'use strict';

// Protected oracle: restored from the baseline commit before scoring.
const test = require('node:test');
const assert = require('node:assert/strict');

const report = require('../report.js');

function sampleUsers() {
  return [
    { id: 3, name: 'Cy', active: true, deletedAt: null },
    { id: 1, name: 'Ada', active: false, deletedAt: null },
    { id: 2, name: 'Bo', active: true, deletedAt: '2026-01-05' },
    { id: 5, name: 'Eve', active: true, deletedAt: null },
    { id: 4, name: 'Dee', active: true },
  ];
}

test('the report still lists active users, one per line, in input order', () => {
  assert.equal(report.activeUserReport(sampleUsers()), '3: Cy\n5: Eve\n4: Dee');
  assert.equal(report.activeUserReport([]), '');
});

test('filterActive is exported as a function', () => {
  assert.equal(typeof report.filterActive, 'function');
});

test('filterActive keeps active, not-deleted users in their original order', () => {
  const kept = report.filterActive(sampleUsers());
  assert.deepEqual(
    kept.map((user) => user.id),
    [3, 5, 4],
  );
});

test('filterActive is pure: it returns a new array and leaves its input alone', () => {
  const users = sampleUsers();
  const before = structuredClone(users);
  const kept = report.filterActive(users);
  assert.notEqual(kept, users);
  assert.deepEqual(users, before);
});

test('filterActive returns an empty array for no users', () => {
  assert.deepEqual(report.filterActive([]), []);
});
