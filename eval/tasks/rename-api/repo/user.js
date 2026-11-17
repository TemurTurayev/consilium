'use strict';

// Formats a user as `Name <email>`.
//
// TASK: the public name breaks the package's camelCase convention. Expose the
// function as `formatUser` and keep `format_user` working as a deprecated
// alias. See task.json for the contract; test/user.test.js asserts it.
function format_user(user) {
  return `${user.name} <${user.email}>`;
}

module.exports = { format_user };
