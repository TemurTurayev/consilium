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
pub fn capture_changes(cwd: &Path) -> Result<String> {
    const PER_FILE_CAP: usize = 8 * 1024;
    const TOTAL_BUDGET: usize = 40 * 1024;

    // Tracked changes via diff.
    let diff_out = std::process::Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(cwd)
        .output()?;
    let diff = String::from_utf8_lossy(&diff_out.stdout).into_owned();

    // Untracked files.
    let untracked_out = std::process::Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(cwd)
        .output()?;
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
            (&content[..PER_FILE_CAP], true)
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
            let cut = &entry[..remaining];
            result.push_str(cut);
            result.push_str("\n[truncated]");
            break;
        }

        total += entry.len();
        result.push_str(&entry);
    }

    Ok(result)
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
    fn huge_untracked_file_is_capped() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("big.txt"), "x".repeat(100_000)).unwrap();
        let c = capture_changes(repo.path()).unwrap();
        assert!(c.len() < 50_000);
        assert!(c.contains("truncated"));
    }
}
