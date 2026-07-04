//! Path confinement shared by the WS server ([`crate::server`]) and the MCP
//! server ([`crate::mcp`]): a caller-supplied `cwd` must resolve inside the
//! directory the server was launched in before any agent is allowed to run
//! there. Both callers are driven by untrusted input (a browser page / an LLM
//! conductor reading repo content), so this is a security boundary, not a
//! convenience check.

/// True if `requested` resolves to a path inside `root` (the dir the server
/// was launched in). Both sides are canonicalized, so `..` traversals and
/// symlinks are resolved before the containment check; a path that cannot be
/// canonicalized (e.g. doesn't exist) is rejected. Prevents a caller from
/// pointing write-enabled agents (or the auto-run verifier) outside the
/// server's working tree.
pub(crate) fn cwd_within_root(requested: &std::path::Path, root: &std::path::Path) -> bool {
    match (requested.canonicalize(), root.canonicalize()) {
        (Ok(req), Ok(rt)) => req.starts_with(&rt),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_itself_is_allowed() {
        let root = tempfile::tempdir().unwrap();
        assert!(cwd_within_root(root.path(), root.path()));
    }

    #[test]
    fn subdir_is_allowed() {
        let root = tempfile::tempdir().unwrap();
        let sub = root.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert!(cwd_within_root(&sub, root.path()));
    }

    #[test]
    fn parent_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().parent().unwrap();
        assert!(!cwd_within_root(parent, root.path()));
    }

    #[test]
    fn unrelated_temp_dir_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        assert!(!cwd_within_root(other.path(), root.path()));
    }

    #[test]
    fn nonexistent_path_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("does_not_exist");
        assert!(!cwd_within_root(&missing, root.path()));
    }

    #[test]
    fn dotdot_escape_is_rejected() {
        // Textually under the root, but canonicalizes to the root's parent.
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        let escape = root.path().join("sub").join("..").join("..");
        assert!(!cwd_within_root(&escape, root.path()));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        // A symlink inside the root pointing outside must not pass.
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        assert!(!cwd_within_root(&link, root.path()));
    }
}
