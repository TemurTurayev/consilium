mod common;

use consilium::safety::{
    create_detached_worktree, inspect_repository, remove_worktree, source_is_applyable,
    RepositoryKind,
};
use std::fs;

#[test]
fn edits_happen_only_in_detached_worktree() {
    let repo = common::committed_repo();
    let state = tempfile::tempdir().unwrap();
    let prepared = create_detached_worktree(repo.path(), state.path()).unwrap();

    fs::write(prepared.path.join("base.txt"), "worker\n").unwrap();

    assert_eq!(
        fs::read_to_string(repo.path().join("base.txt")).unwrap(),
        "base\n"
    );
    assert_eq!(
        prepared.base_commit,
        common::git_output(repo.path(), &["rev-parse", "HEAD"])
    );
    remove_worktree(&prepared).unwrap();
    assert!(!prepared.path.exists());
}

#[test]
fn repository_subdirectory_resolves_to_canonical_root() {
    let repo = common::committed_repo();
    let nested = repo.path().join("nested").join("deeper");
    fs::create_dir_all(&nested).unwrap();
    let state = tempfile::tempdir().unwrap();

    let prepared = create_detached_worktree(&nested, state.path()).unwrap();

    assert_eq!(prepared.source_repo, repo.path().canonicalize().unwrap());
    assert_eq!(
        inspect_repository(&nested).unwrap().git_root.as_deref(),
        Some(repo.path().canonicalize().unwrap().to_str().unwrap())
    );
    remove_worktree(&prepared).unwrap();
}

#[test]
fn dirty_tracked_source_uses_head_and_preserves_operator_bytes() {
    let repo = common::committed_repo();
    fs::write(repo.path().join("base.txt"), "operator dirty\n").unwrap();
    let before = fs::read(repo.path().join("base.txt")).unwrap();
    let state = tempfile::tempdir().unwrap();

    let prepared = create_detached_worktree(repo.path(), state.path()).unwrap();

    assert_eq!(fs::read(prepared.path.join("base.txt")).unwrap(), b"base\n");
    assert_eq!(fs::read(repo.path().join("base.txt")).unwrap(), before);
    assert!(!source_is_applyable(repo.path(), &prepared.base_commit).unwrap());
    remove_worktree(&prepared).unwrap();
    assert_eq!(fs::read(repo.path().join("base.txt")).unwrap(), before);
}

#[test]
fn untracked_source_file_is_absent_from_worktree_and_preserved() {
    let repo = common::committed_repo();
    let untracked = repo.path().join("operator notes.txt");
    fs::write(&untracked, b"do not copy\n").unwrap();
    let state = tempfile::tempdir().unwrap();

    let prepared = create_detached_worktree(repo.path(), state.path()).unwrap();

    assert!(!prepared.path.join("operator notes.txt").exists());
    assert_eq!(fs::read(&untracked).unwrap(), b"do not copy\n");
    assert!(!source_is_applyable(repo.path(), &prepared.base_commit).unwrap());
    remove_worktree(&prepared).unwrap();
    assert_eq!(fs::read(&untracked).unwrap(), b"do not copy\n");
}

#[test]
fn clean_source_at_base_is_applyable() {
    let repo = common::committed_repo();
    let base = common::git_output(repo.path(), &["rev-parse", "HEAD"]);
    assert!(source_is_applyable(repo.path(), &base).unwrap());
}

#[test]
fn tracked_staged_untracked_and_changed_head_are_not_applyable() {
    let tracked = common::committed_repo();
    let base = common::git_output(tracked.path(), &["rev-parse", "HEAD"]);
    fs::write(tracked.path().join("base.txt"), "dirty\n").unwrap();
    assert!(!source_is_applyable(tracked.path(), &base).unwrap());

    let staged = common::committed_repo();
    let base = common::git_output(staged.path(), &["rev-parse", "HEAD"]);
    fs::write(staged.path().join("base.txt"), "staged\n").unwrap();
    common::git(staged.path(), &["add", "--", "base.txt"]);
    assert!(!source_is_applyable(staged.path(), &base).unwrap());

    let untracked = common::committed_repo();
    let base = common::git_output(untracked.path(), &["rev-parse", "HEAD"]);
    fs::write(untracked.path().join("new.txt"), "new\n").unwrap();
    assert!(!source_is_applyable(untracked.path(), &base).unwrap());

    let advanced = common::committed_repo();
    let base = common::git_output(advanced.path(), &["rev-parse", "HEAD"]);
    fs::write(advanced.path().join("next.txt"), "next\n").unwrap();
    common::git(advanced.path(), &["add", "--", "next.txt"]);
    common::commit(advanced.path(), "next");
    assert!(!source_is_applyable(advanced.path(), &base).unwrap());
}

#[test]
fn non_git_and_unborn_repositories_are_rejected() {
    let non_git = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let error = create_detached_worktree(non_git.path(), state.path()).unwrap_err();
    assert!(error.to_string().contains("Git repository"));
    assert!(!source_is_applyable(non_git.path(), "deadbeef").unwrap());

    let unborn = tempfile::tempdir().unwrap();
    common::git(unborn.path(), &["init", "-q"]);
    let error = create_detached_worktree(unborn.path(), state.path()).unwrap_err();
    assert!(error.to_string().contains("commit"));
    assert!(!source_is_applyable(unborn.path(), "deadbeef").unwrap());
    assert_eq!(
        inspect_repository(unborn.path()).unwrap().kind,
        RepositoryKind::Git
    );
}

#[test]
fn remove_worktree_is_idempotent() {
    let repo = common::committed_repo();
    let state = tempfile::tempdir().unwrap();
    let prepared = create_detached_worktree(repo.path(), state.path()).unwrap();

    remove_worktree(&prepared).unwrap();
    remove_worktree(&prepared).unwrap();

    assert!(!prepared.path.exists());
    assert_eq!(
        fs::read_to_string(repo.path().join("base.txt")).unwrap(),
        "base\n"
    );
}

#[test]
fn remove_worktree_succeeds_after_external_directory_removal() {
    let repo = common::committed_repo();
    let state = tempfile::tempdir().unwrap();
    let prepared = create_detached_worktree(repo.path(), state.path()).unwrap();
    fs::remove_dir_all(&prepared.path).unwrap();

    remove_worktree(&prepared).unwrap();
    remove_worktree(&prepared).unwrap();

    assert_eq!(
        fs::read_to_string(repo.path().join("base.txt")).unwrap(),
        "base\n"
    );
}

#[test]
fn paths_with_spaces_and_dash_prefixed_components_work() {
    let outer = tempfile::tempdir().unwrap();
    let repo_path = outer.path().join("repo with spaces").join("-source");
    fs::create_dir_all(&repo_path).unwrap();
    common::git(&repo_path, &["init", "-q"]);
    fs::write(repo_path.join("base.txt"), "base\n").unwrap();
    common::git(&repo_path, &["add", "--", "base.txt"]);
    common::commit(&repo_path, "base");
    let state = outer.path().join("state with spaces").join("-runs");

    let prepared = create_detached_worktree(&repo_path, &state).unwrap();

    assert!(prepared.path.join("base.txt").is_file());
    remove_worktree(&prepared).unwrap();
}

#[cfg(unix)]
#[test]
fn symlinked_state_root_is_rejected_without_touching_target() {
    use std::os::unix::fs::symlink;

    let repo = common::committed_repo();
    let outer = tempfile::tempdir().unwrap();
    let target = outer.path().join("target");
    fs::create_dir(&target).unwrap();
    let state_link = outer.path().join("state-link");
    symlink(&target, &state_link).unwrap();

    assert!(create_detached_worktree(repo.path(), &state_link).is_err());
    assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
    assert_eq!(
        fs::read_to_string(repo.path().join("base.txt")).unwrap(),
        "base\n"
    );
}
