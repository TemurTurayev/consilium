//! Integrity of the built-in eval suite (`eval/tasks`). Zero quota: no model is
//! called. Each fixture is judged by its own verifier, on the same prepared
//! repository and with the same scoring a live trial uses
//! (`eval::prepare_trial_repo` + `eval::score_tree`).
//!
//! For every task it proves that the starter repo FAILS its own check and that
//! the task's reference `solution.patch` makes the very same check PASS without
//! touching the protected tests — so the task is solvable, the baseline is not
//! already solved, and the failure is in the code rather than a missing tool
//! (the identical command passes once the patch is applied).
//!
//! The `fixture_suite_check_*` tests at the bottom are controls: they feed the
//! same check a broken fixture and require it to object.
//!
//! Needs `cargo`, `python3` and `node` (18+) on PATH.

use consilium::orchestrator::eval::{self, EvalTask, VerificationStatus, SKIPPED_DIRS};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

const TASK_NAMES: [&str; 12] = [
    "add-greeting",
    "clamp-retry",
    "dedupe-stable",
    "filter-active",
    "fix-sum",
    "json-content-type",
    "normalize-email",
    "parse-duration",
    "parse-kv",
    "path-extension",
    "rename-api",
    "split-validation",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("core/ has a parent")
        .to_path_buf()
}

fn suite_dir() -> PathBuf {
    repo_root().join("eval/tasks")
}

fn solution_patch(task: &EvalTask) -> PathBuf {
    task.repo_dir
        .parent()
        .expect("repo/ sits inside the task directory")
        .join("solution.patch")
}

/// Fails loudly when a runtime the fixtures need is absent, so a missing tool
/// is never mistaken for a fixture that "fails as intended".
fn require_tools() {
    for tool in ["cargo", "python3", "node"] {
        let found = Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success());
        assert!(
            found,
            "the eval fixture suite needs `{tool}` on PATH (Rust, Python 3 and Node 18+ fixtures)"
        );
    }
}

/// Everything under a fixture that must not be committed: symlinks (a trial
/// copy skips them), build or dependency output, and editor/OS litter.
fn fixture_problems(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![format!("unreadable directory: {}", dir.display())];
    };
    entries
        .filter_map(Result::ok)
        .flat_map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = entry.file_type().expect("file type is readable");
            if file_type.is_symlink() {
                vec![format!("symlink: {}", path.display())]
            } else if file_type.is_dir() && SKIPPED_DIRS.contains(&name.as_str()) {
                vec![format!("build or dependency output: {}", path.display())]
            } else if file_type.is_dir() {
                fixture_problems(&path)
            } else if name.ends_with(".pyc") || name == ".DS_Store" {
                vec![format!("generated file: {}", path.display())]
            } else {
                Vec::new()
            }
        })
        .collect()
}

/// The paths a patch touches, read from the patch itself (`git apply --numstat`).
fn patched_paths(repo: &Path, patch: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(["apply", "--numstat"])
        .arg(patch)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("running git apply --numstat: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "solution.patch does not parse: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split('\t').nth(2))
        .map(str::to_string)
        .collect())
}

fn is_protected(task: &EvalTask, path: &str) -> bool {
    task.protected_paths
        .iter()
        .any(|protected| path == protected || path.starts_with(&format!("{protected}/")))
}

/// The integrity check for one task. `Ok` only if the starter fails its own
/// verifier, the reference patch applies cleanly, leaves the protected tests
/// alone, and makes the same verifier pass.
async fn check_reference_solution(task: &EvalTask) -> Result<(), String> {
    let patch = solution_patch(task);
    if !patch.is_file() {
        return Err("has no solution.patch".into());
    }
    let repo = eval::prepare_trial_repo(task).map_err(|e| format!("{e:#}"))?;

    let touched = patched_paths(repo.path(), &patch)?;
    if touched.is_empty() {
        return Err("solution.patch changes nothing".into());
    }
    if let Some(path) = touched.iter().find(|path| is_protected(task, path)) {
        return Err(format!("solution.patch edits the protected path {path}"));
    }

    let baseline = eval::score_tree(repo.path(), task)
        .await
        .map_err(|e| format!("{e:#}"))?;
    if baseline != VerificationStatus::Failed {
        return Err(format!(
            "the starter must fail its own check, but verification was {baseline:?}"
        ));
    }

    let applied = Command::new("git")
        .arg("apply")
        .arg(&patch)
        .current_dir(repo.path())
        .output()
        .map_err(|e| format!("running git apply: {e}"))?;
    if !applied.status.success() {
        return Err(format!(
            "solution.patch does not apply to the starter: {}",
            String::from_utf8_lossy(&applied.stderr).trim()
        ));
    }

    let solved = eval::score_tree(repo.path(), task)
        .await
        .map_err(|e| format!("{e:#}"))?;
    if solved != VerificationStatus::Passed {
        return Err(format!(
            "the reference solution must pass the task's check, but verification was {solved:?}"
        ));
    }
    Ok(())
}

/// Shape problems that need no toolchain: metadata, files, hygiene.
fn manifest_problems(task: &EvalTask) -> Vec<String> {
    let mut problems = Vec::new();
    let dir_name = task
        .repo_dir
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if task.name != dir_name {
        problems.push(format!(
            "is named '{}' but lives in '{dir_name}'",
            task.name
        ));
    }
    if !["bugfix", "parsing", "api", "refactor"].contains(&task.category.as_str()) {
        problems.push(format!("unknown category '{}'", task.category));
    }
    if !["rust", "python", "node"].contains(&task.language.as_str()) {
        problems.push(format!("unknown language '{}'", task.language));
    }
    if !["small", "medium"].contains(&task.difficulty.as_str()) {
        problems.push(format!("unknown difficulty '{}'", task.difficulty));
    }
    if task.prompt.trim().is_empty() {
        problems.push("has an empty prompt".into());
    }
    if task.verify.as_ref().and_then(|v| v.test.as_ref()).is_none() {
        problems.push("declares no verify.test command".into());
    }
    if task.protected_paths.is_empty() {
        problems.push("protects no test file".into());
    }
    for path in &task.protected_paths {
        if !task.repo_dir.join(path).is_file() {
            problems.push(format!("protected path {path} is not a file in repo/"));
        }
    }
    if !solution_patch(task).is_file() {
        problems.push("has no solution.patch".into());
    }
    problems.extend(fixture_problems(&task.repo_dir));
    problems
}

fn label(task: &EvalTask, problems: Vec<String>) -> Vec<String> {
    problems
        .into_iter()
        .map(|problem| format!("{}: {problem}", task.name))
        .collect()
}

#[tokio::test]
async fn fixture_suite_has_twelve_valid_reference_solutions() {
    require_tools();
    let tasks = eval::load_suite(&suite_dir(), None).unwrap();

    let names: Vec<&str> = tasks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, TASK_NAMES, "the suite is exactly these twelve tasks");
    let categories: HashSet<&str> = tasks.iter().map(|t| t.category.as_str()).collect();
    assert_eq!(
        categories,
        HashSet::from(["bugfix", "parsing", "api", "refactor"])
    );
    let languages: HashSet<&str> = tasks.iter().map(|t| t.language.as_str()).collect();
    assert_eq!(languages, HashSet::from(["rust", "python", "node"]));

    let shape: Vec<String> = tasks
        .iter()
        .flat_map(|task| label(task, manifest_problems(task)))
        .collect();
    assert!(shape.is_empty(), "fixture problems:\n{}", shape.join("\n"));

    // Every starter fails its own check; every reference patch fixes it.
    let checks = futures::future::join_all(tasks.iter().map(check_reference_solution)).await;
    let failures: Vec<String> = tasks
        .iter()
        .zip(checks)
        .filter_map(|(task, check)| check.err().map(|e| format!("{}: {e}", task.name)))
        .collect();
    assert!(
        failures.is_empty(),
        "reference solutions that do not hold:\n{}",
        failures.join("\n")
    );
}

#[test]
fn fixture_suite_dry_run_lists_all_twelve_tasks_and_spends_nothing() {
    let cwd = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_consilium"))
        .arg("eval")
        .arg("--suite")
        .arg(suite_dir())
        .current_dir(cwd.path())
        .env("HOME", home.path())
        .env("PATH", "/nonexistent")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("no quota spent"), "{stdout}");
    assert!(stdout.contains("12 task(s)"), "{stdout}");
    for name in TASK_NAMES {
        assert!(
            stdout.contains(&format!("  - {name} (")),
            "missing {name}: {stdout}"
        );
    }
}

// ── Controls: the check must be able to fail ─────────────────────────────────

const GOOD_PATCH: &str = "--- a/value.txt\n+++ b/value.txt\n@@ -1 +1 @@\n-wrong\n+right\n";

/// A one-task suite scored by `grep -q right value.txt`, with `check.txt` as
/// its protected file.
fn control_task(starter_value: &str, patch: Option<&str>) -> (tempfile::TempDir, EvalTask) {
    let suite = tempfile::tempdir().unwrap();
    let task_dir = suite.path().join("control");
    let repo = task_dir.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("value.txt"), format!("{starter_value}\n")).unwrap();
    std::fs::write(repo.join("check.txt"), "oracle\n").unwrap();
    std::fs::write(
        task_dir.join("task.json"),
        serde_json::json!({
            "name": "control",
            "category": "bugfix",
            "language": "rust",
            "difficulty": "small",
            "prompt": "make value.txt say right",
            "verify": { "test": "grep -q right value.txt" },
            "protected_paths": ["check.txt"],
        })
        .to_string(),
    )
    .unwrap();
    if let Some(patch) = patch {
        std::fs::write(task_dir.join("solution.patch"), patch).unwrap();
    }
    let task = eval::load_suite(suite.path(), None)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    (suite, task)
}

async fn control_error(starter_value: &str, patch: Option<&str>) -> String {
    let (_suite, task) = control_task(starter_value, patch);
    check_reference_solution(&task)
        .await
        .expect_err("the integrity check accepted a broken fixture")
}

#[tokio::test]
async fn fixture_suite_check_accepts_a_sound_fixture() {
    let (_suite, task) = control_task("wrong", Some(GOOD_PATCH));
    assert_eq!(check_reference_solution(&task).await, Ok(()));
    assert_eq!(manifest_problems(&task), Vec::<String>::new());
}

#[tokio::test]
async fn fixture_suite_check_rejects_a_patch_that_does_not_fix_the_task() {
    let useless = GOOD_PATCH.replace("+right", "+still-wrong");
    let error = control_error("wrong", Some(&useless)).await;
    assert!(error.contains("must pass"), "got: {error}");
}

#[tokio::test]
async fn fixture_suite_check_rejects_a_starter_that_already_passes() {
    // The starter already satisfies the check, so there is nothing to solve.
    let patch = "--- a/value.txt\n+++ b/value.txt\n@@ -1 +1,2 @@\n right\n+extra\n";
    let error = control_error("right", Some(patch)).await;
    assert!(error.contains("must fail"), "got: {error}");
}

#[tokio::test]
async fn fixture_suite_check_rejects_a_patch_that_edits_the_protected_test() {
    let cheating =
        format!("{GOOD_PATCH}--- a/check.txt\n+++ b/check.txt\n@@ -1 +1 @@\n-oracle\n+rewritten\n");
    let error = control_error("wrong", Some(&cheating)).await;
    assert!(error.contains("protected path check.txt"), "got: {error}");
}

#[tokio::test]
async fn fixture_suite_check_rejects_a_missing_or_stale_patch() {
    let error = control_error("wrong", None).await;
    assert!(error.contains("no solution.patch"), "got: {error}");

    // A patch written against different starter content does not apply.
    let stale = GOOD_PATCH.replace("-wrong", "-something-else");
    let error = control_error("wrong", Some(&stale)).await;
    assert!(error.contains("does not apply"), "got: {error}");
}

#[test]
fn fixture_suite_hygiene_check_flags_build_output_and_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("target/debug")).unwrap();
    std::fs::create_dir_all(dir.path().join("src/__pycache__")).unwrap();
    std::fs::write(dir.path().join("src/mod.pyc"), "").unwrap();
    std::os::unix::fs::symlink("/etc/hosts", dir.path().join("link")).unwrap();

    let problems = fixture_problems(dir.path()).join("\n");
    for expected in ["target", "__pycache__", "mod.pyc", "symlink"] {
        assert!(
            problems.contains(expected),
            "missing {expected} in:\n{problems}"
        );
    }
    // a clean tree raises nothing
    let clean = tempfile::tempdir().unwrap();
    std::fs::write(clean.path().join("lib.rs"), "").unwrap();
    assert_eq!(fixture_problems(clean.path()), Vec::<String>::new());
}
