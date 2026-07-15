mod common;

use consilium::safety::{
    create_detached_worktree, inspect_repository, remove_worktree, reopen_prepared_worktree,
    source_is_applyable, PreparedWorktreeSummary, RepositoryKind,
};
use std::fs;

#[cfg(unix)]
fn executable_script(path: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

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
fn remove_worktree_succeeds_after_external_removal_and_prune() {
    let repo = common::committed_repo();
    let state = tempfile::tempdir().unwrap();
    let prepared = create_detached_worktree(repo.path(), state.path()).unwrap();
    fs::remove_dir_all(&prepared.path).unwrap();
    common::git(repo.path(), &["worktree", "prune", "--expire", "now"]);

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

#[cfg(unix)]
fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn state_root_inside_source_is_rejected_before_any_mutation() {
    let repo = common::committed_repo();
    let before_status = common::git_output(repo.path(), &["status", "--porcelain=v1"]);
    let before_bytes = fs::read(repo.path().join("base.txt")).unwrap();
    #[cfg(unix)]
    let before_mode = mode(repo.path());
    let nested_state = repo.path().join(".consilium").join("state");

    let error = create_detached_worktree(repo.path(), &nested_state).unwrap_err();

    assert!(error.to_string().contains("outside the source repository"));
    assert!(!nested_state.exists());
    assert_eq!(
        common::git_output(repo.path(), &["status", "--porcelain=v1"]),
        before_status
    );
    assert_eq!(
        fs::read(repo.path().join("base.txt")).unwrap(),
        before_bytes
    );
    #[cfg(unix)]
    assert_eq!(mode(repo.path()), before_mode);
}

#[cfg(unix)]
#[test]
fn existing_symlink_component_cannot_hide_state_root_inside_source() {
    use std::os::unix::fs::symlink;

    let repo = common::committed_repo();
    let outer = tempfile::tempdir().unwrap();
    let alias = outer.path().join("source-alias");
    symlink(repo.path(), &alias).unwrap();
    let nested_state = alias.join("not-yet-created").join("state");

    let error = create_detached_worktree(repo.path(), &nested_state).unwrap_err();

    assert!(error.to_string().contains("outside the source repository"));
    assert!(!repo.path().join("not-yet-created").exists());
    assert!(common::git_output(repo.path(), &["status", "--porcelain=v1"]).is_empty());
}

#[test]
fn source_root_itself_cannot_be_used_as_state_root() {
    let repo = common::committed_repo();
    let before_status = common::git_output(repo.path(), &["status", "--porcelain=v1"]);
    let before_bytes = fs::read(repo.path().join("base.txt")).unwrap();
    #[cfg(unix)]
    let before_mode = mode(repo.path());

    let error = create_detached_worktree(repo.path(), repo.path()).unwrap_err();

    assert!(error.to_string().contains("outside the source repository"));
    assert_eq!(
        common::git_output(repo.path(), &["status", "--porcelain=v1"]),
        before_status
    );
    assert_eq!(
        fs::read(repo.path().join("base.txt")).unwrap(),
        before_bytes
    );
    #[cfg(unix)]
    assert_eq!(mode(repo.path()), before_mode);
}

#[test]
fn corrupted_handle_cannot_remove_an_unrelated_registered_worktree() {
    let repo = common::committed_repo();
    let state = tempfile::tempdir().unwrap();
    let first = create_detached_worktree(repo.path(), state.path()).unwrap();
    let second = create_detached_worktree(repo.path(), state.path()).unwrap();
    let mut corrupted = first.clone();
    corrupted.id = second.id.clone();
    corrupted.path = second.path.clone();

    assert!(remove_worktree(&corrupted).is_err());
    assert!(second.path.join("base.txt").is_file());

    remove_worktree(&first).unwrap();
    remove_worktree(&second).unwrap();
}

#[test]
fn serialized_prepared_worktree_exposes_only_the_documented_fields() {
    let repo = common::committed_repo();
    let state = tempfile::tempdir().unwrap();
    let prepared = create_detached_worktree(repo.path(), state.path()).unwrap();

    let value = serde_json::to_value(&prepared).unwrap();
    let object = value.as_object().unwrap();
    let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(keys, ["base_commit", "id", "path", "source_repo"]);

    remove_worktree(&prepared).unwrap();
}

#[cfg(unix)]
#[test]
fn persisted_summary_can_be_securely_reopened_after_process_boundary() {
    let repo = common::committed_repo();
    let state = tempfile::tempdir().unwrap();
    let prepared = create_detached_worktree(repo.path(), state.path()).unwrap();
    let summary_file = tempfile::NamedTempFile::new().unwrap();
    fs::write(
        summary_file.path(),
        serde_json::to_vec(&PreparedWorktreeSummary::from(&prepared)).unwrap(),
    )
    .unwrap();
    drop(prepared);

    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("cross_process_reopen_helper")
        .arg("--nocapture")
        .env("CONSILIUM_REOPEN_STATE", state.path())
        .env("CONSILIUM_REOPEN_SUMMARY", summary_file.path())
        .status()
        .unwrap();

    assert!(status.success());
    assert_eq!(
        fs::read_dir(state.path().join("worktrees"))
            .unwrap()
            .count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn cross_process_reopen_helper() {
    let Some(state) = std::env::var_os("CONSILIUM_REOPEN_STATE") else {
        return;
    };
    let summary_path = std::env::var_os("CONSILIUM_REOPEN_SUMMARY").unwrap();
    let summary: PreparedWorktreeSummary =
        serde_json::from_slice(&fs::read(summary_path).unwrap()).unwrap();
    let reopened = reopen_prepared_worktree(std::path::Path::new(&state), &summary).unwrap();
    remove_worktree(&reopened).unwrap();
}

#[cfg(unix)]
#[test]
fn forged_summary_cannot_reopen_cleanup_authority() {
    let repo = common::committed_repo();
    let state = tempfile::tempdir().unwrap();
    let prepared = create_detached_worktree(repo.path(), state.path()).unwrap();
    let valid = PreparedWorktreeSummary::from(&prepared);

    let mut forged = valid.clone();
    forged.base_commit = "0000000000000000000000000000000000000000".into();
    assert!(reopen_prepared_worktree(state.path(), &forged).is_err());

    let mut forged = valid.clone();
    forged.id = "a".repeat(32);
    forged.path = state.path().join("worktrees").join(&forged.id);
    assert!(reopen_prepared_worktree(state.path(), &forged).is_err());

    let other_state = tempfile::tempdir().unwrap();
    assert!(reopen_prepared_worktree(other_state.path(), &valid).is_err());
    assert!(prepared.path.join("base.txt").is_file());
    remove_worktree(&prepared).unwrap();
}

#[cfg(unix)]
#[test]
fn permissive_existing_state_root_is_rejected_without_chmod() {
    use std::os::unix::fs::PermissionsExt;

    let repo = common::committed_repo();
    let outer = tempfile::tempdir().unwrap();
    let state = outer.path().join("existing-state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(state.join("unrelated.txt"), "preserve").unwrap();

    let error = create_detached_worktree(repo.path(), &state).unwrap_err();

    assert!(error.to_string().contains("owner-only"));
    assert_eq!(mode(&state), 0o755);
    assert_eq!(
        fs::read_to_string(state.join("unrelated.txt")).unwrap(),
        "preserve"
    );
}

#[cfg(unix)]
#[test]
fn raw_materialization_preserves_executable_symlink_and_gitlink_without_filters() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let repo = common::committed_repo();
    fs::write(repo.path().join("tool.sh"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        repo.path().join("tool.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    symlink("base.txt", repo.path().join("base-link")).unwrap();
    common::git(repo.path(), &["add", "--", "tool.sh", "base-link"]);
    common::commit(repo.path(), "raw tree entries");
    let head = common::git_output(repo.path(), &["rev-parse", "HEAD"]);
    common::git(
        repo.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},vendor"),
        ],
    );
    common::commit(repo.path(), "gitlink");
    let state = tempfile::tempdir().unwrap();

    let prepared = create_detached_worktree(repo.path(), state.path()).unwrap();

    assert_eq!(mode(&prepared.path.join("tool.sh")) & 0o111, 0o100);
    assert_eq!(
        fs::read_link(prepared.path.join("base-link")).unwrap(),
        std::path::Path::new("base.txt")
    );
    assert!(prepared.path.join("vendor").is_dir());
    remove_worktree(&prepared).unwrap();
}

#[cfg(unix)]
#[test]
fn repository_post_checkout_hook_cannot_mutate_source() {
    let repo = common::committed_repo();
    let hook = repo.path().join(".git/hooks/post-checkout");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    executable_script(
        &hook,
        &format!(
            "printf 'hooked\\n' > {}",
            shell_quote(&repo.path().join("base.txt"))
        ),
    );
    let state = tempfile::tempdir().unwrap();

    let prepared = create_detached_worktree(repo.path(), state.path()).unwrap();

    assert_eq!(
        fs::read_to_string(repo.path().join("base.txt")).unwrap(),
        "base\n"
    );
    assert!(common::git_output(repo.path(), &["status", "--porcelain=v1"]).is_empty());
    remove_worktree(&prepared).unwrap();
}

#[cfg(unix)]
#[test]
fn repository_smudge_filter_is_not_executed() {
    let repo = common::committed_repo();
    let marker = repo.path().join("smudge-ran");
    let script = repo.path().join("evil-smudge.sh");
    executable_script(
        &script,
        &format!("printf ran > {}; cat", shell_quote(&marker)),
    );
    fs::write(repo.path().join(".gitattributes"), "base.txt filter=evil\n").unwrap();
    common::git(repo.path(), &["add", "--", ".gitattributes"]);
    common::commit(repo.path(), "attributes");
    common::git(
        repo.path(),
        &["config", "filter.evil.smudge", script.to_str().unwrap()],
    );
    let state = tempfile::tempdir().unwrap();

    let prepared = create_detached_worktree(repo.path(), state.path()).unwrap();

    assert!(!marker.exists());
    assert_eq!(
        fs::read_to_string(prepared.path.join("base.txt")).unwrap(),
        "base\n"
    );
    remove_worktree(&prepared).unwrap();
}

#[cfg(unix)]
#[test]
fn repository_process_filter_is_not_executed() {
    let repo = common::committed_repo();
    let marker = repo.path().join("process-ran");
    let script = repo.path().join("evil-process.sh");
    executable_script(
        &script,
        &format!("printf ran > {}; exit 1", shell_quote(&marker)),
    );
    fs::write(repo.path().join(".gitattributes"), "base.txt filter=evil\n").unwrap();
    common::git(repo.path(), &["add", "--", ".gitattributes"]);
    common::commit(repo.path(), "attributes");
    common::git(
        repo.path(),
        &["config", "filter.evil.process", script.to_str().unwrap()],
    );
    let state = tempfile::tempdir().unwrap();

    let prepared = create_detached_worktree(repo.path(), state.path()).unwrap();

    assert!(!marker.exists());
    assert_eq!(
        fs::read_to_string(prepared.path.join("base.txt")).unwrap(),
        "base\n"
    );
    remove_worktree(&prepared).unwrap();
}

#[cfg(unix)]
#[test]
fn repository_fsmonitor_is_not_executed_during_inspection() {
    let repo = common::committed_repo();
    let commands = tempfile::tempdir().unwrap();
    let marker = commands.path().join("fsmonitor-inspect-ran");
    let script = commands.path().join("evil-fsmonitor.sh");
    executable_script(
        &script,
        &format!("printf ran > {}; printf '\\n'", shell_quote(&marker)),
    );
    common::git(
        repo.path(),
        &["config", "core.fsmonitor", script.to_str().unwrap()],
    );

    let inspected = inspect_repository(repo.path()).unwrap();

    assert!(inspected.clean);
    assert!(!marker.exists());
    assert_eq!(
        fs::read_to_string(repo.path().join("base.txt")).unwrap(),
        "base\n"
    );
}

#[cfg(unix)]
#[test]
fn repository_fsmonitor_is_not_executed_during_applyability_check() {
    let repo = common::committed_repo();
    let commands = tempfile::tempdir().unwrap();
    let marker = commands.path().join("fsmonitor-apply-ran");
    let script = commands.path().join("evil-fsmonitor.sh");
    executable_script(
        &script,
        &format!("printf ran > {}; printf '\\n'", shell_quote(&marker)),
    );
    common::git(
        repo.path(),
        &["config", "core.fsmonitor", script.to_str().unwrap()],
    );
    let base = common::git_output(repo.path(), &["rev-parse", "HEAD"]);

    assert!(source_is_applyable(repo.path(), &base).unwrap());
    assert!(!marker.exists());
    assert_eq!(
        fs::read_to_string(repo.path().join("base.txt")).unwrap(),
        "base\n"
    );
}

#[test]
fn locked_registration_is_preserved_and_reported() {
    let repo = common::committed_repo();
    let state = tempfile::tempdir().unwrap();
    let prepared = create_detached_worktree(repo.path(), state.path()).unwrap();
    common::git(
        repo.path(),
        &["worktree", "lock", "--", prepared.path.to_str().unwrap()],
    );

    let error = remove_worktree(&prepared).unwrap_err();

    assert!(error.to_string().contains("registered"));
    assert!(
        common::git_output(repo.path(), &["worktree", "list", "--porcelain"])
            .contains(&prepared.path.display().to_string())
    );
    assert!(fs::read_dir(state.path().join("worktrees"))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .any(|entry| entry.path().join("base.txt").is_file()));
}

#[cfg(unix)]
#[test]
fn git_root_ending_in_newline_preserves_the_path_byte() {
    let outer = tempfile::tempdir().unwrap();
    let repo_path = outer.path().join("repository\n");
    fs::create_dir(&repo_path).unwrap();
    common::git(&repo_path, &["init", "-q"]);
    fs::write(repo_path.join("base.txt"), "base\n").unwrap();
    common::git(&repo_path, &["add", "--", "base.txt"]);
    common::commit(&repo_path, "base");

    let inspected = inspect_repository(&repo_path).unwrap();

    assert_eq!(
        inspected.git_root.as_deref(),
        Some(repo_path.canonicalize().unwrap().to_str().unwrap())
    );
}
