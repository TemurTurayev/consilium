'use strict';

// Builds the "active users" report, one `id: name` line per active user.
//
// TASK (refactor): the rule for who counts as active is tangled into the
// formatting loop, so nothing else can reuse it. Extract it into an exported
// pure function `filterActive(users)` and make `activeUserReport` use it. The
// report's output must not change. See test/report.test.js.
function activeUserReport(users) {
  const lines = [];
  for (const user of users) {
    if (user.active && !user.deletedAt) {
      lines.push(`${user.id}: ${user.name}`);
    }
  }
  return lines.join('\n');
}

module.exports = { activeUserReport };
