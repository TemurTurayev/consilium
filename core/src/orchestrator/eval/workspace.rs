//! The isolated repository every trial runs in, and the independent scoring of
//! what an approach leaves behind. Shared by live trials and by the suite's
//! zero-quota integrity test, so both judge a fixture exactly the same way.

use super::report::{ReportMeta, SuiteReport, VerificationStatus};
use super::EvalTask;
use crate::orchestrator::verify::run_verify;
use anyhow::Context;
use std::path::Path;

/// Directories never copied out of a fixture: version control, build output
/// and dependency caches.
pub const SKIPPED_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "__pycache__",
    "dist",
];

/// A fresh, isolated copy of a task's starter repo with a baseline commit —
/// what every trial starts from. Dropping the returned directory deletes it.
pub fn prepare_trial_repo(task: &EvalTask) -> anyhow::Result<tempfile::TempDir> {
    let tmp = tempfile::tempdir().context("creating trial tempdir")?;
    copy_dir(&task.repo_dir, tmp.path())
        .with_context(|| format!("copying starter repo for task '{}'", task.name))?;
    git_init_commit(tmp.path()).context("git-initializing the trial repo")?;
    Ok(tmp)
}

/// Score a trial repo with the task's own verifier — the external oracle. The
/// task's protected (test) files are first restored from the baseline commit,
/// so an approach cannot pass by deleting or rewriting the test it is judged on.
pub async fn score_tree(cwd: &Path, task: &EvalTask) -> anyhow::Result<VerificationStatus> {
    restore_protected(cwd, &task.protected_paths)?;
    let outcome = run_verify(cwd, task.verify.as_ref()).await;
    Ok(VerificationStatus::from_verify(outcome.ran, outcome.passed))
}

/// Recursively copy `src` into `dst`, skipping [`SKIPPED_DIRS`] and not
/// following symlinks (avoids escaping the fixture and symlink-loop recursion).
fn copy_dir(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if SKIPPED_DIRS.iter().any(|s| name == *s) {
            continue;
        }
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        if ft.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Restore the task's protected (test/oracle) files from the baseline commit,
/// undoing any worker edits.
fn restore_protected(cwd: &Path, paths: &[String]) -> anyhow::Result<()> {
    for p in paths {
        run_git(cwd, &["checkout", "HEAD", "--", p])
            .with_context(|| format!("restoring protected path '{p}'"))?;
    }
    Ok(())
}

/// `git init` + baseline commit so `capture_changes` (git diff HEAD) works.
fn git_init_commit(cwd: &Path) -> anyhow::Result<()> {
    run_git(cwd, &["init", "-q"])?;
    run_git(cwd, &["add", "-A"])?;
    run_git(cwd, &["commit", "-q", "-m", "baseline", "--allow-empty"])?;
    Ok(())
}

fn git(cwd: &Path, args: &[&str]) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "consilium-eval")
        .env("GIT_AUTHOR_EMAIL", "eval@consilium.local")
        .env("GIT_COMMITTER_NAME", "consilium-eval")
        .env("GIT_COMMITTER_EMAIL", "eval@consilium.local");
    command
}

fn run_git(cwd: &Path, args: &[&str]) -> anyhow::Result<()> {
    let status = git(cwd, args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| format!("spawning git {args:?}"))?;
    if !status.success() {
        anyhow::bail!("git {:?} failed in {}", args, cwd.display());
    }
    Ok(())
}

/// Trimmed stdout of a git command, or `None` if it could not run or failed.
fn git_stdout(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = git(cwd, args)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The commit a suite directory was at, `-dirty` appended when it has
/// uncommitted changes, or `unknown` when it is not inside a git repository.
pub fn suite_commit(suite_dir: &Path) -> String {
    let Some(commit) = git_stdout(suite_dir, &["rev-parse", "HEAD"]).filter(|c| !c.is_empty())
    else {
        return "unknown".to_string();
    };
    let dirty = git_stdout(suite_dir, &["status", "--porcelain", "--", "."])
        .is_some_and(|changes| !changes.is_empty());
    if dirty {
        format!("{commit}-dirty")
    } else {
        commit
    }
}

/// Format unix seconds as an RFC 3339 UTC timestamp (`2026-11-17T12:00:00Z`).
/// Civil-from-days (Howard Hinnant's algorithm) — no date crate needed.
pub fn utc_timestamp(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let second_of_day = unix_seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3_600,
        second_of_day % 3_600 / 60,
        second_of_day % 60
    )
}

/// Write `report` as pretty JSON to `path`, creating its directory. The file
/// is replaced whole (written beside it, then renamed), so a reader — or a run
/// killed mid-save — never sees a half-written report.
pub fn save_report(path: &Path, report: &SuiteReport) -> anyhow::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(report).context("serializing the eval report")?;
    let mut staging = path.as_os_str().to_owned();
    staging.push(".tmp");
    let staging = std::path::PathBuf::from(staging);
    std::fs::write(&staging, json).with_context(|| format!("writing {}", staging.display()))?;
    std::fs::rename(&staging, path).with_context(|| format!("saving {}", path.display()))
}

impl ReportMeta {
    /// Record when a run starts and which commit its suite is at. I/O: reads
    /// the clock and asks git about `suite_dir`.
    pub fn capture(suite_dir: &Path) -> Self {
        Self {
            generated_at: utc_timestamp(crate::quota::unix_now()),
            suite_commit: suite_commit(suite_dir),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_timestamp_formats_known_instants() {
        assert_eq!(utc_timestamp(0), "1970-01-01T00:00:00Z");
        // a leap day
        assert_eq!(utc_timestamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_timestamp(1_794_916_254), "2026-11-17T11:50:54Z");
        // the last second of a year
        assert_eq!(utc_timestamp(1_798_761_599), "2026-12-31T23:59:59Z");
    }

    #[test]
    fn suite_commit_is_unknown_outside_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(suite_commit(dir.path()), "unknown");
    }

    #[test]
    fn suite_commit_names_the_commit_and_flags_uncommitted_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("task.json"), "{}").unwrap();
        git_init_commit(dir.path()).unwrap();

        let clean = suite_commit(dir.path());
        assert_eq!(clean.len(), 40, "a full commit id, got {clean}");
        assert!(clean.chars().all(|c| c.is_ascii_hexdigit()), "{clean}");

        std::fs::write(dir.path().join("task.json"), "{\"edited\":true}").unwrap();
        assert_eq!(suite_commit(dir.path()), format!("{clean}-dirty"));
    }
}
