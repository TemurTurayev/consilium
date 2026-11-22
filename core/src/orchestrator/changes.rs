use anyhow::Result;
use std::path::Path;

/// Captures the working-tree changes in the git repository at `cwd`:
/// - `git diff HEAD` for tracked modifications/deletions.
/// - `git ls-files --others --exclude-standard` for untracked files, each
///   appended as `--- new file: <path> ---\n<content>` with an 8 KiB per-file
///   cap (`\n[truncated]` when capped) and a ~40 KiB total budget.
/// - Returns `"(no changes)"` when both outputs are empty.
///
/// Never touches the git index (no `git add`). Pure `std::process`, no new deps.
/// Byte-budget truncation that never splits a UTF-8 code point: walks the cut
/// back to the nearest char boundary (a raw `&s[..max]` panics mid-codepoint —
/// worker-written files are frequently non-ASCII).
fn truncate_at_boundary(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub fn capture_changes(cwd: &Path) -> Result<String> {
    const PER_FILE_CAP: usize = 8 * 1024;
    const TOTAL_BUDGET: usize = 40 * 1024;

    // Tracked changes via diff. A failed git invocation (e.g. cwd is not a
    // repository) must surface as an error — a silent "(no changes)" would let
    // the conductor accept a worker that did nothing.
    let diff_out = std::process::Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(cwd)
        .output()?;
    if !diff_out.status.success() {
        anyhow::bail!(
            "git diff HEAD failed in {} (exit {}): {}",
            cwd.display(),
            diff_out.status,
            String::from_utf8_lossy(&diff_out.stderr).trim()
        );
    }
    let diff = String::from_utf8_lossy(&diff_out.stdout).into_owned();

    // Untracked files.
    let untracked_out = std::process::Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(cwd)
        .output()?;
    if !untracked_out.status.success() {
        anyhow::bail!(
            "git ls-files failed in {} (exit {}): {}",
            cwd.display(),
            untracked_out.status,
            String::from_utf8_lossy(&untracked_out.stderr).trim()
        );
    }
    let untracked_list = String::from_utf8_lossy(&untracked_out.stdout).into_owned();

    if diff.trim().is_empty() && untracked_list.trim().is_empty() {
        return Ok("(no changes)".to_string());
    }

    let mut result = diff;
    let mut total = result.len();

    for path_str in untracked_list.lines() {
        let path_str = path_str.trim();
        if path_str.is_empty() {
            continue;
        }
        if total >= TOTAL_BUDGET {
            break;
        }

        let full_path = cwd.join(path_str);
        let content = match std::fs::read_to_string(&full_path) {
            Ok(c) => c,
            Err(_) => continue, // binary or unreadable — skip silently
        };

        let (content_to_use, truncated) = if content.len() > PER_FILE_CAP {
            (truncate_at_boundary(&content, PER_FILE_CAP), true)
        } else {
            (content.as_str(), false)
        };

        let mut entry = format!("--- new file: {path_str} ---\n{content_to_use}");
        if truncated {
            entry.push_str("\n[truncated]");
        }

        // Enforce total budget.
        let remaining = TOTAL_BUDGET.saturating_sub(total);
        if entry.len() > remaining {
            result.push_str(truncate_at_boundary(&entry, remaining));
            result.push_str("\n[truncated]");
            break;
        }

        total += entry.len();
        result.push_str(&entry);
    }

    Ok(result)
}

/// More untracked files than this and a tree is not snapshotted.
const MAX_UNTRACKED_FILES: usize = 2000;

/// Snapshots of one working tree, and the differences between them — how a
/// run's own changes are told apart from whatever was already uncommitted.
///
/// A snapshot is the tree as a git tree object: tracked and untracked files
/// alike, minus what `.gitignore` excludes. It is built in a temporary index
/// and a temporary object store that only *reads* the repository's, so the
/// repository — its index, its objects, its working tree — is left exactly as
/// it was. (An untracked file may be a secret nobody has ignored yet; it must
/// not be copied into `.git`.) Everything is discarded with the value.
///
/// git runs hardened (`safety::hardened_git`): the tree was just edited by an
/// agent that reads untrusted content, and a hook it left behind must not run
/// inside Consilium.
pub struct TreeSnapshots {
    cwd: std::path::PathBuf,
    /// The temporary index and object store.
    scratch: tempfile::TempDir,
    /// The repository's own object store — read, never written.
    repository_objects: std::path::PathBuf,
}

impl TreeSnapshots {
    /// Fails when `cwd` is not inside a git repository.
    pub fn open(cwd: &Path) -> Result<Self> {
        let objects = hardened_stdout(crate::safety::hardened_git(cwd).args([
            "rev-parse",
            "--git-path",
            "objects",
        ]))?;
        let scratch = tempfile::tempdir()?;
        std::fs::create_dir(scratch.path().join("objects"))?;
        Ok(Self {
            cwd: cwd.to_path_buf(),
            // git answers relative to `cwd`, or absolutely inside a worktree.
            repository_objects: cwd.join(objects.trim()),
            scratch,
        })
    }

    /// The tree as it is now, as the id of a tree object.
    ///
    /// A snapshot reads every untracked file. A tree with more than
    /// `MAX_UNTRACKED_FILES` of them — a build directory nobody ignored — is
    /// refused rather than read twice per run.
    pub fn take(&self) -> Result<String> {
        // Counted against the repository's own index: in the temporary one,
        // empty at first, every file would look untracked.
        let untracked = hardened_stdout(crate::safety::hardened_git(&self.cwd).args([
            "ls-files",
            "--others",
            "--exclude-standard",
        ]))?;
        let untracked = untracked.lines().count();
        if untracked > MAX_UNTRACKED_FILES {
            anyhow::bail!(
                "{untracked} untracked files in {} — too many to snapshot (limit {MAX_UNTRACKED_FILES})",
                self.cwd.display()
            );
        }
        // The temporary index is kept between snapshots, so a later one only
        // reads the files that changed since.
        self.git(&["add", "--all"])?;
        Ok(self.git(&["write-tree"])?.trim().to_string())
    }

    /// What changed between two snapshots, as a patch — new and deleted files
    /// included. `"(no changes)"` when they are the same tree. Past ~40 KiB
    /// the patch is cut, under a summary that still names every file touched.
    pub fn changes_between(&self, before: &str, after: &str) -> Result<String> {
        const TOTAL_BUDGET: usize = 40 * 1024;
        let diff = |format: &[&str]| {
            let args: Vec<&str> = ["diff", "--no-color", "--no-ext-diff", "--no-textconv"]
                .into_iter()
                .chain(format.iter().copied())
                .chain([before, after])
                .collect();
            self.git(&args)
        };
        let patch = diff(&[])?;
        if patch.is_empty() {
            return Ok("(no changes)".to_string());
        }
        if patch.len() <= TOTAL_BUDGET {
            return Ok(patch);
        }
        Ok(format!(
            "{}\n{}\n[truncated]",
            diff(&["--stat"])?,
            truncate_at_boundary(&patch, TOTAL_BUDGET)
        ))
    }

    /// Hardened git against the temporary index and object store.
    fn git(&self, args: &[&str]) -> Result<String> {
        hardened_stdout(
            crate::safety::hardened_git(&self.cwd)
                .args(args)
                .env("GIT_INDEX_FILE", self.scratch.path().join("index"))
                .env("GIT_OBJECT_DIRECTORY", self.scratch.path().join("objects"))
                .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &self.repository_objects),
        )
    }
}

/// The top-level directory of the repository `cwd` is in, or `None` outside one.
pub fn repository_root(cwd: &Path) -> Option<std::path::PathBuf> {
    hardened_stdout(crate::safety::hardened_git(cwd).args(["rev-parse", "--show-toplevel"]))
        .ok()
        .map(|root| root.trim().to_string())
        .filter(|root| !root.is_empty())
        .map(std::path::PathBuf::from)
}

/// Runs a prepared git command and returns its stdout as written; a non-zero
/// exit is an error carrying git's own message.
fn hardened_stdout(git: &mut std::process::Command) -> Result<String> {
    let out = git.output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git failed (exit {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Read-only list of repo-relative paths with uncommitted changes (modified,
/// added, deleted, or untracked), sorted and deduped. Backs the worker
/// blackboard's "files modified this run" signal. Best-effort: callers degrade
/// to an empty list on error — this is cosmetic context, never load-bearing
/// (unlike `capture_changes`, whose failure must surface).
pub fn capture_changed_files(cwd: &Path) -> Result<Vec<String>> {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(cwd)
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "git status failed in {} (exit {}): {}",
            cwd.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut files: Vec<String> = text
        .lines()
        .filter_map(|line| {
            // porcelain v1: two status chars + a space + the path. Renames are
            // "R  old -> new" — keep the post-arrow path. Paths with special
            // chars are git-quoted.
            let path = line.get(3..)?.trim();
            if path.is_empty() {
                return None;
            }
            let path = path.rsplit(" -> ").next().unwrap_or(path);
            Some(path.trim_matches('"').to_string())
        })
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &std::path::Path, args: &[&str]) {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap()
                .status
                .success(),
            "git {:?} failed",
            args
        );
    }

    fn temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        // Hermetic identity: bare CI runners have no global user.name/email,
        // and `git commit` refuses to run without one.
        git(dir.path(), &["config", "user.name", "consilium-test"]);
        git(
            dir.path(),
            &["config", "user.email", "test@consilium.local"],
        );
        git(dir.path(), &["commit", "--allow-empty", "-m", "init", "-q"]);
        dir
    }

    #[test]
    fn captures_tracked_modifications() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("a.txt"), "v1\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "add a", "-q"]);
        std::fs::write(repo.path().join("a.txt"), "v2\n").unwrap();
        let c = capture_changes(repo.path()).unwrap();
        assert!(c.contains("-v1"));
        assert!(c.contains("+v2"));
    }

    #[test]
    fn captures_untracked_files_with_content() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("new.rs"), "fn x() {}\n").unwrap();
        let c = capture_changes(repo.path()).unwrap();
        assert!(c.contains("new.rs"));
        assert!(c.contains("fn x() {}"));
    }

    #[test]
    fn clean_tree_reports_no_changes() {
        let repo = temp_repo();
        let c = capture_changes(repo.path()).unwrap();
        assert!(c.contains("(no changes)"));
    }

    #[test]
    fn non_git_directory_is_an_error_not_no_changes() {
        let dir = tempfile::tempdir().unwrap();
        let err = capture_changes(dir.path()).unwrap_err();
        assert!(err.to_string().contains("git diff HEAD failed"));
    }

    #[test]
    fn huge_untracked_file_is_capped() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("big.txt"), "x".repeat(100_000)).unwrap();
        let c = capture_changes(repo.path()).unwrap();
        assert!(c.len() < 50_000);
        assert!(c.contains("truncated"));
    }

    #[test]
    fn multibyte_content_truncates_without_panicking() {
        let repo = temp_repo();
        // 3-byte chars ensure the byte cap lands mid-codepoint without the guard.
        std::fs::write(repo.path().join("cyr.txt"), "ж".repeat(40_000)).unwrap();
        let c = capture_changes(repo.path()).unwrap();
        assert!(c.contains("truncated"));
        assert!(c.contains('ж')); // content survived, boundary-safe
    }

    // ── snapshots ────────────────────────────────────────────────────────────

    fn porcelain(repo: &tempfile::TempDir) -> String {
        let out = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    /// Every file under the repository's `.git`, with its size — what a
    /// snapshot must leave exactly as it found it.
    fn git_dir_listing(repo: &tempfile::TempDir) -> Vec<(std::path::PathBuf, u64)> {
        fn walk(dir: &std::path::Path, found: &mut Vec<(std::path::PathBuf, u64)>) {
            for entry in std::fs::read_dir(dir).unwrap().map(Result::unwrap) {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, found);
                } else {
                    found.push((path, entry.metadata().unwrap().len()));
                }
            }
        }
        let mut found = Vec::new();
        walk(&repo.path().join(".git"), &mut found);
        found.sort();
        found
    }

    #[test]
    fn two_snapshots_of_an_unchanged_tree_are_the_same_tree() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("untracked.rs"), "fn x() {}\n").unwrap();
        let snapshots = TreeSnapshots::open(repo.path()).unwrap();
        let first = snapshots.take().unwrap();
        let second = snapshots.take().unwrap();
        assert_eq!(first, second);
        assert_eq!(
            snapshots.changes_between(&first, &second).unwrap(),
            "(no changes)"
        );
    }

    #[test]
    fn the_difference_between_snapshots_is_what_happened_in_between() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("tracked.rs"), "v1\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "add", "-q"]);
        std::fs::write(repo.path().join("before.rs"), "already here\n").unwrap();
        let snapshots = TreeSnapshots::open(repo.path()).unwrap();
        let before = snapshots.take().unwrap();

        std::fs::write(repo.path().join("added.rs"), "fn added() {}\n").unwrap();
        std::fs::write(repo.path().join("tracked.rs"), "v2\n").unwrap();
        std::fs::remove_file(repo.path().join("before.rs")).unwrap();
        let after = snapshots.take().unwrap();

        let diff = snapshots.changes_between(&before, &after).unwrap();
        assert!(diff.contains("+fn added() {}"), "{diff}");
        assert!(diff.contains("-v1") && diff.contains("+v2"), "{diff}");
        assert!(diff.contains("-already here"), "a deletion shows: {diff}");
    }

    /// The diff is handed to the conductor, who may apply it: `git apply`
    /// rejects a patch whose last line has lost its newline.
    #[test]
    fn the_diff_between_snapshots_is_a_whole_patch() {
        let repo = temp_repo();
        let snapshots = TreeSnapshots::open(repo.path()).unwrap();
        let before = snapshots.take().unwrap();
        std::fs::write(repo.path().join("added.rs"), "fn added() {}\n").unwrap();
        let after = snapshots.take().unwrap();

        let diff = snapshots.changes_between(&before, &after).unwrap();
        assert!(diff.ends_with("+fn added() {}\n"), "{diff:?}");
    }

    /// One generated file can fill the whole budget. Whatever is cut, the
    /// conductor must still see every file the worker touched.
    #[test]
    fn a_truncated_diff_still_lists_every_file() {
        let repo = temp_repo();
        let snapshots = TreeSnapshots::open(repo.path()).unwrap();
        let before = snapshots.take().unwrap();
        let huge: String = (0..8000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(repo.path().join("a-generated.lock"), huge).unwrap();
        std::fs::write(repo.path().join("z-real-change.rs"), "fn z() {}\n").unwrap();
        let after = snapshots.take().unwrap();

        let diff = snapshots.changes_between(&before, &after).unwrap();
        assert!(diff.len() < 48 * 1024, "{}", diff.len());
        assert!(diff.contains("[truncated]"));
        assert!(diff.contains("a-generated.lock"), "{}", &diff[..400]);
        assert!(diff.contains("z-real-change.rs"), "{}", &diff[..400]);
    }

    #[test]
    fn a_snapshot_stages_nothing() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("untracked.rs"), "fn x() {}\n").unwrap();
        let status = porcelain(&repo);
        TreeSnapshots::open(repo.path()).unwrap().take().unwrap();
        assert_eq!(porcelain(&repo), status);
        assert_eq!(status, "?? untracked.rs\n");
    }

    /// An untracked file may be a secret nobody has ignored yet. Hashing it
    /// into the repository's object store would keep a copy there until git's
    /// next housekeeping — long after the file itself is gone.
    #[test]
    fn a_snapshot_writes_nothing_into_the_repository() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("tracked.rs"), "v1\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "add", "-q"]);
        std::fs::write(repo.path().join("tracked.rs"), "v2\n").unwrap();
        std::fs::write(repo.path().join("unignored.env"), "TOKEN=x\n").unwrap();
        let untouched = git_dir_listing(&repo);

        let snapshots = TreeSnapshots::open(repo.path()).unwrap();
        let before = snapshots.take().unwrap();
        std::fs::write(repo.path().join("more.rs"), "fn more() {}\n").unwrap();
        let after = snapshots.take().unwrap();
        snapshots.changes_between(&before, &after).unwrap();

        assert_eq!(git_dir_listing(&repo), untouched);
    }

    /// The tree was just edited by an agent that reads untrusted content. A
    /// hook it left behind must not run inside Consilium.
    #[cfg(unix)]
    #[test]
    fn a_snapshot_runs_no_hook_of_the_repository() {
        use std::os::unix::fs::PermissionsExt;
        let repo = temp_repo();
        let marker = repo.path().join("hook-ran");
        let hooks = repo.path().join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        for hook in ["post-index-change", "pre-commit", "post-checkout"] {
            let path = hooks.join(hook);
            std::fs::write(&path, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(repo.path().join("new.rs"), "fn x() {}\n").unwrap();

        TreeSnapshots::open(repo.path()).unwrap().take().unwrap();

        assert!(
            !marker.exists(),
            "a repository hook ran during the snapshot"
        );
    }

    #[test]
    fn a_snapshot_leaves_ignored_files_out() {
        let repo = temp_repo();
        std::fs::write(repo.path().join(".gitignore"), "secret.env\n").unwrap();
        let snapshots = TreeSnapshots::open(repo.path()).unwrap();
        let before = snapshots.take().unwrap();
        std::fs::write(repo.path().join("secret.env"), "TOKEN=x\n").unwrap();
        let after = snapshots.take().unwrap();
        assert_eq!(before, after);
    }

    /// A tree with a build directory nobody ignored would have every file in
    /// it read twice per worker run. Past a bound the snapshot is refused, and
    /// the caller falls back to the plain diff.
    #[test]
    fn a_tree_with_too_many_untracked_files_is_not_snapshotted() {
        let repo = temp_repo();
        let dir = repo.path().join("build");
        std::fs::create_dir(&dir).unwrap();
        for i in 0..=MAX_UNTRACKED_FILES {
            std::fs::write(dir.join(format!("{i}.o")), "").unwrap();
        }
        let snapshots = TreeSnapshots::open(repo.path()).unwrap();
        let err = snapshots.take().unwrap_err().to_string();
        assert!(err.contains("untracked"), "{err}");
    }

    #[test]
    fn snapshots_need_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        assert!(TreeSnapshots::open(dir.path()).is_err());
        assert_eq!(repository_root(dir.path()), None);
    }

    #[test]
    fn capture_changed_files_lists_modified_and_untracked() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("tracked.txt"), "v1\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "add", "-q"]);
        std::fs::write(repo.path().join("tracked.txt"), "v2\n").unwrap(); // modified
        std::fs::write(repo.path().join("untracked.rs"), "fn x() {}\n").unwrap(); // untracked
        let files = capture_changed_files(repo.path()).unwrap();
        assert!(files.contains(&"tracked.txt".to_string()), "got {files:?}");
        assert!(files.contains(&"untracked.rs".to_string()), "got {files:?}");
    }

    #[test]
    fn capture_changed_files_empty_on_clean_tree() {
        let repo = temp_repo();
        assert!(capture_changed_files(repo.path()).unwrap().is_empty());
    }

    #[test]
    fn capture_changed_files_lists_deleted() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("gone.txt"), "x\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "add", "-q"]);
        std::fs::remove_file(repo.path().join("gone.txt")).unwrap();
        let files = capture_changed_files(repo.path()).unwrap();
        assert!(files.contains(&"gone.txt".to_string()), "got {files:?}");
    }

    #[test]
    fn capture_changed_files_lists_renamed_target() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("old.txt"), "x\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "add", "-q"]);
        git(repo.path(), &["mv", "old.txt", "new.txt"]); // staged rename → "R  old -> new"
        let files = capture_changed_files(repo.path()).unwrap();
        // The post-arrow (new) path is what a worker should see.
        assert!(files.contains(&"new.txt".to_string()), "got {files:?}");
    }
}
