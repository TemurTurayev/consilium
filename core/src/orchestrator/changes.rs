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
