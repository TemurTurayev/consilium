// TASK: `extension` should return a path's final extension in lowercase, and
// None where there is none. This first draft just takes whatever follows the
// last '.', which is wrong for dotfiles, names without a dot, dots in a
// directory name and upper-case extensions. See task.json for the contract;
// tests/extension.rs asserts it — fix the function here, do not edit the test.
pub fn extension(path: &str) -> Option<String> {
    path.rsplit('.').next().map(str::to_string)
}
