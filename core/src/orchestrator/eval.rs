//! M-eval: a benchmark harness that measures whether Consilium's orchestration
//! (council + build/test grounding) actually beats a solo agent.
//!
//! Each task is run through one or more **approaches**; the harness then scores
//! the result with an **independent** `run_verify` on the produced tree. The
//! approach's own "I completed" is recorded but is NOT the score — a run that
//! reports success yet leaves a broken build scores `false`. A trial where no
//! verifier ran counts as not-passed (a conservative lower bound, surfaced
//! separately as "unscored"). This external-oracle rule is the honesty keystone.
//!
//! The harness never touches the real quota ledger: each trial uses a fresh
//! in-memory [`QuotaStore`], so token deltas are isolated and a benchmark run
//! does not pollute `~/.consilium/usage.db`.

use crate::adapters::RunRequest;
use crate::config::VerifyConfig;
use crate::event::Provider;
use crate::orchestrator::conduct::{run_conduct, ConductDeps};
use crate::orchestrator::resilience::{run_with_failover, ModelHealth, Rung};
use crate::orchestrator::verify::run_verify;
use crate::quota::QuotaStore;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The orchestration configurations under comparison. Each isolates one variable
/// so a pairwise comparison is an honest ablation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Approach {
    /// One worker model on the full prompt (no decompose, no gates). The baseline.
    Solo,
    /// The full pipeline: decompose → workers → supervisor → review → arbiter →
    /// build/test grounding.
    Conduct,
    /// `Conduct` with grounding (internal build/test gate) disabled.
    ConductNoGrounding,
    /// `Conduct` routing each diff to a different-family reviewer/arbiter.
    ConductCrossFamily,
}

impl Approach {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "solo" => Some(Self::Solo),
            "conduct" => Some(Self::Conduct),
            "conduct-no-grounding" => Some(Self::ConductNoGrounding),
            "conduct-cross-family" => Some(Self::ConductCrossFamily),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Solo => "solo",
            Self::Conduct => "conduct",
            Self::ConductNoGrounding => "conduct-no-grounding",
            Self::ConductCrossFamily => "conduct-cross-family",
        }
    }

    /// Rough per-trial model-call estimate, for the dry-run cost warning.
    pub fn call_estimate(&self) -> &'static str {
        match self {
            Self::Solo => "~1 model call",
            _ => "~5-15+ model calls (decompose + per-subtask worker/eval/review/arbiter)",
        }
    }

    fn is_conduct_family(&self) -> bool {
        !matches!(self, Self::Solo)
    }
}

/// Parse a comma-separated approach list (`solo,conduct,...`).
pub fn parse_approaches(csv: &str) -> anyhow::Result<Vec<Approach>> {
    let mut out = Vec::new();
    for tok in csv.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let a = Approach::parse(tok)
            .with_context(|| format!("unknown approach '{tok}' (valid: solo, conduct, conduct-no-grounding, conduct-cross-family)"))?;
        if !out.contains(&a) {
            out.push(a);
        }
    }
    if out.is_empty() {
        anyhow::bail!("no approaches selected");
    }
    Ok(out)
}

/// A benchmark task loaded from `eval/tasks/<name>/task.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct EvalTask {
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub context: String,
    /// Build/test/lint commands used BOTH as the conduct approach's internal
    /// grounding gate AND as the harness's external scorer. Omitted ⇒ autodetect.
    #[serde(default)]
    pub verify: Option<VerifyConfig>,
    /// Files restored from the baseline commit before scoring (the test/oracle),
    /// so an approach cannot pass by deleting or rewriting the test it is judged
    /// on. Paths are relative to the task repo (e.g. `tests/greeting.rs`).
    #[serde(default)]
    pub protected_paths: Vec<String>,
    /// Filled in by [`load_suite`]: the task's starter `repo/`, copied per trial.
    #[serde(skip)]
    pub repo_dir: PathBuf,
}

/// Provides the per-approach orchestration dependencies. The real CLI builds
/// these from `Config`; tests inject scripted ladders (zero quota).
pub trait EvalDeps {
    /// One worker's failover ladder, run on the full prompt for the solo arm.
    fn solo_ladder(&self) -> Vec<Rung>;
    /// A fresh `ConductDeps` with the given internal grounding config and
    /// cross-family-review flag.
    fn conduct_deps(&self, verify: Option<VerifyConfig>, cross_family: bool) -> ConductDeps;
}

/// The outcome of running one approach (before external scoring).
struct ApproachRun {
    /// The approach's own pipeline completion — informational, NOT the score.
    pipeline_ok: bool,
    error: Option<String>,
}

/// One scored trial.
#[derive(Debug, Clone, Serialize)]
pub struct TrialResult {
    pub task: String,
    pub approach: String,
    pub trial: u32,
    /// THE SCORE: an independent `run_verify` ran AND passed on the result tree.
    pub success: bool,
    /// Whether a verifier actually ran (no command resolved ⇒ could-not-score).
    pub verify_ran: bool,
    /// The approach's own reported completion (informational, not the score).
    pub pipeline_ok: bool,
    pub tokens: u64,
    pub wall_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Per (task, approach) summary over N trials.
#[derive(Debug, Clone, Serialize)]
pub struct CellAggregate {
    pub task: String,
    pub approach: String,
    pub passed: u32,
    pub total: u32,
    /// Trials where no verifier ran (reported apart from the pass rate).
    pub unscored: u32,
    /// All trials agreed (all pass or all fail).
    pub stable: bool,
    pub median_tokens: u64,
    pub median_wall_ms: u64,
}

/// Per-approach summary across all tasks.
#[derive(Debug, Clone, Serialize)]
pub struct ApproachAggregate {
    pub approach: String,
    pub passed: u32,
    pub total: u32,
    /// Trials where no verifier ran (kept distinct from real failures).
    pub unscored: u32,
    pub median_tokens: u64,
    pub median_wall_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Aggregate {
    pub per_task_approach: Vec<CellAggregate>,
    pub per_approach: Vec<ApproachAggregate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuiteReport {
    pub trials: u32,
    pub results: Vec<TrialResult>,
    pub aggregate: Aggregate,
}

/// Load every task under `suite_dir` (immediate subdirs with a `task.json` +
/// `repo/`). `filter` keeps only tasks whose name contains the substring.
pub fn load_suite(suite_dir: &Path, filter: Option<&str>) -> anyhow::Result<Vec<EvalTask>> {
    let entries = std::fs::read_dir(suite_dir)
        .with_context(|| format!("reading suite dir {}", suite_dir.display()))?;
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let mut tasks = Vec::new();
    for dir in dirs {
        let manifest = dir.join("task.json");
        if !manifest.exists() {
            continue;
        }
        let raw = std::fs::read_to_string(&manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;
        let mut task: EvalTask = serde_json::from_str(&raw)
            .with_context(|| format!("parsing {}", manifest.display()))?;
        task.repo_dir = dir.join("repo");
        if !task.repo_dir.is_dir() {
            anyhow::bail!(
                "task '{}' has no repo/ directory at {}",
                task.name,
                task.repo_dir.display()
            );
        }
        if let Some(f) = filter {
            if !task.name.contains(f) {
                continue;
            }
        }
        tasks.push(task);
    }
    Ok(tasks)
}

/// A human-readable plan for `--spend-quota`-off mode. Calls no models.
pub fn dry_run_plan(tasks: &[EvalTask], approaches: &[Approach], trials: u32) -> String {
    let runs = tasks.len() as u32 * approaches.len() as u32 * trials;
    let mut s = String::new();
    s.push_str("DRY RUN — no quota spent. Pass --spend-quota to actually call models.\n\n");
    s.push_str(&format!(
        "{} task(s) × {} approach(es) × {} trial(s) = {} runs\n\ntasks:\n",
        tasks.len(),
        approaches.len(),
        trials,
        runs
    ));
    for t in tasks {
        s.push_str(&format!("  - {}\n", t.name));
    }
    s.push_str("\napproaches (rough per-trial cost):\n");
    for a in approaches {
        s.push_str(&format!("  - {} — {}\n", a.as_str(), a.call_estimate()));
    }
    s.push_str("\nSolo is cheapest; full Conduct is the most expensive. Multiply per-trial cost × trials × tasks.\n");
    s
}

/// Run the full matrix (task × approach × trial). Each trial is isolated (fresh
/// temp repo + in-memory quota). Spends real quota via `deps`.
pub async fn run_suite(
    tasks: &[EvalTask],
    approaches: &[Approach],
    trials: u32,
    deps: &dyn EvalDeps,
    timeout: Duration,
) -> anyhow::Result<SuiteReport> {
    let mut results = Vec::new();
    for task in tasks {
        for &approach in approaches {
            for trial in 0..trials {
                let r = run_trial(task, approach, trial, deps, timeout).await?;
                eprintln!(
                    "  {} / {} / trial {} → {} ({} tok, {} ms)",
                    r.task,
                    r.approach,
                    r.trial,
                    if r.success { "PASS" } else { "fail" },
                    r.tokens,
                    r.wall_ms,
                );
                results.push(r);
            }
        }
    }
    let aggregate = aggregate(&results);
    Ok(SuiteReport {
        trials,
        results,
        aggregate,
    })
}

/// One isolated trial: copy the starter repo, run the approach, score externally.
async fn run_trial(
    task: &EvalTask,
    approach: Approach,
    trial: u32,
    deps: &dyn EvalDeps,
    timeout: Duration,
) -> anyhow::Result<TrialResult> {
    let tmp = tempfile::tempdir().context("creating trial tempdir")?;
    let cwd = tmp.path().to_path_buf();
    copy_dir(&task.repo_dir, &cwd)
        .with_context(|| format!("copying starter repo for task '{}'", task.name))?;
    git_init_commit(&cwd).context("git-initializing the trial repo")?;

    // Isolated ledger — never touches the real usage.db.
    let quota = QuotaStore::open_in_memory().context("opening in-memory quota store")?;

    let start = Instant::now();
    let run = run_approach(approach, task, cwd.clone(), deps, &quota, timeout).await;
    let wall_ms = start.elapsed().as_millis() as u64;

    // Undo any tampering with the scored test files before judging — an approach
    // must not be able to pass by deleting or rewriting the test it is judged on.
    restore_protected(&cwd, &task.protected_paths)?;

    // External, independent score — the honesty keystone. Deliberately left OUT of
    // wall_ms so the metric measures orchestration, not scoring. (NB: a grounded
    // conduct arm runs build/test once internally too, so its tree is verified
    // twice per trial — once as its gate, once here as the score.)
    let v = run_verify(&cwd, task.verify.as_ref()).await;
    let success = v.ran && v.passed;

    Ok(TrialResult {
        task: task.name.clone(),
        approach: approach.as_str().to_string(),
        trial,
        success,
        verify_ran: v.ran,
        pipeline_ok: run.pipeline_ok,
        tokens: total_tokens(&quota)?,
        wall_ms,
        error: run.error,
    })
}

/// Execute one approach against the prepared `cwd`. A fresh `ModelHealth` per
/// run; the approach never aborts the suite — an error is captured as a fail.
async fn run_approach(
    approach: Approach,
    task: &EvalTask,
    cwd: PathBuf,
    deps: &dyn EvalDeps,
    quota: &QuotaStore,
    timeout: Duration,
) -> ApproachRun {
    let health = ModelHealth::new();

    if approach == Approach::Solo {
        let ladder = deps.solo_ladder();
        let prompt = task.prompt.clone();
        let cwd2 = cwd.clone();
        let res = run_with_failover(
            &ladder,
            "solo",
            move |model| RunRequest {
                prompt: prompt.clone(),
                model,
                cwd: cwd2.clone(),
                advisory: false,
                write: true,
            },
            quota,
            &health,
            timeout,
        )
        .await;
        // run_with_failover returns Ok only when a rung ran to completion.
        return match res {
            Ok(_) => ApproachRun {
                pipeline_ok: true,
                error: None,
            },
            Err(e) => ApproachRun {
                pipeline_ok: false,
                error: Some(e.to_string()),
            },
        };
    }

    debug_assert!(approach.is_conduct_family());
    let grounding = approach != Approach::ConductNoGrounding;
    let cross_family = approach == Approach::ConductCrossFamily;
    // Grounding on ⇒ conduct's internal gate uses the task's own build/test.
    let verify = if grounding { task.verify.clone() } else { None };
    let deps_c = deps.conduct_deps(verify, cross_family);

    match run_conduct(
        &task.prompt,
        &task.context,
        deps_c,
        quota,
        cwd,
        timeout,
        &health,
    )
    .await
    {
        Ok(o) => ApproachRun {
            pipeline_ok: o.failed.is_none() && o.halted.is_none(),
            error: None,
        },
        Err(e) => ApproachRun {
            pipeline_ok: false,
            error: Some(e.to_string()),
        },
    }
}

/// Sum input+output tokens across all providers from a per-trial store.
fn total_tokens(quota: &QuotaStore) -> anyhow::Result<u64> {
    let mut sum = 0u64;
    for p in [Provider::Claude, Provider::Codex, Provider::Gemini] {
        let (i, o) = quota.totals_since(p, 0)?;
        sum += i + o;
    }
    Ok(sum)
}

/// Recursively copy `src` into `dst`, skipping VCS/build/dependency dirs and not
/// following symlinks (avoids escaping the fixture and symlink-loop recursion).
fn copy_dir(src: &Path, dst: &Path) -> anyhow::Result<()> {
    const SKIP: &[&str] = &[
        ".git",
        "target",
        "node_modules",
        ".venv",
        "__pycache__",
        "dist",
    ];
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if SKIP.iter().any(|s| name == *s) {
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
/// undoing any worker edits, so an approach cannot game the score by deleting or
/// rewriting the test it is judged against.
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

fn run_git(cwd: &Path, args: &[&str]) -> anyhow::Result<()> {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "consilium-eval")
        .env("GIT_AUTHOR_EMAIL", "eval@consilium.local")
        .env("GIT_COMMITTER_NAME", "consilium-eval")
        .env("GIT_COMMITTER_EMAIL", "eval@consilium.local")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| format!("spawning git {args:?}"))?;
    if !status.success() {
        anyhow::bail!("git {:?} failed in {}", args, cwd.display());
    }
    Ok(())
}

fn median(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2
    }
}

/// Aggregate trial results into per-cell and per-approach summaries. Pure.
pub fn aggregate(results: &[TrialResult]) -> Aggregate {
    use std::collections::BTreeMap;

    let mut cells: BTreeMap<(String, String), Vec<&TrialResult>> = BTreeMap::new();
    let mut by_approach: BTreeMap<String, Vec<&TrialResult>> = BTreeMap::new();
    for r in results {
        cells
            .entry((r.task.clone(), r.approach.clone()))
            .or_default()
            .push(r);
        by_approach.entry(r.approach.clone()).or_default().push(r);
    }

    let per_task_approach = cells
        .into_iter()
        .map(|((task, approach), trs)| {
            let passed = trs.iter().filter(|r| r.success).count() as u32;
            let unscored = trs.iter().filter(|r| !r.verify_ran).count() as u32;
            let stable = trs.iter().all(|r| r.success) || trs.iter().all(|r| !r.success);
            CellAggregate {
                task,
                approach,
                passed,
                total: trs.len() as u32,
                unscored,
                stable,
                median_tokens: median(&trs.iter().map(|r| r.tokens).collect::<Vec<_>>()),
                median_wall_ms: median(&trs.iter().map(|r| r.wall_ms).collect::<Vec<_>>()),
            }
        })
        .collect();

    let per_approach = by_approach
        .into_iter()
        .map(|(approach, trs)| ApproachAggregate {
            approach,
            passed: trs.iter().filter(|r| r.success).count() as u32,
            total: trs.len() as u32,
            unscored: trs.iter().filter(|r| !r.verify_ran).count() as u32,
            median_tokens: median(&trs.iter().map(|r| r.tokens).collect::<Vec<_>>()),
            median_wall_ms: median(&trs.iter().map(|r| r.wall_ms).collect::<Vec<_>>()),
        })
        .collect();

    Aggregate {
        per_task_approach,
        per_approach,
    }
}

/// Render a `SuiteReport` as a human markdown table.
pub fn markdown_report(report: &SuiteReport) -> String {
    let mut s = String::new();
    s.push_str(&format!("# M-eval results (N={})\n\n", report.trials));
    s.push_str("| Task | Approach | Pass (k/N) | Stable | Median tok | Median ms | Unscored |\n");
    s.push_str("|---|---|---|---|---|---|---|\n");
    for c in &report.aggregate.per_task_approach {
        s.push_str(&format!(
            "| {} | {} | {}/{} | {} | {} | {} | {} |\n",
            c.task,
            c.approach,
            c.passed,
            c.total,
            // 'stable' is meaningless for a single sample.
            if c.total < 2 {
                "n/a"
            } else if c.stable {
                "yes"
            } else {
                "**no**"
            },
            c.median_tokens,
            c.median_wall_ms,
            c.unscored,
        ));
    }
    s.push_str("\n## Per-approach overall\n\n");
    s.push_str(
        "| Approach | Pass (k/N) | Unscored | Median tok | Median ms |\n|---|---|---|---|---|\n",
    );
    for a in &report.aggregate.per_approach {
        s.push_str(&format!(
            "| {} | {}/{} | {} | {} | {} |\n",
            a.approach, a.passed, a.total, a.unscored, a.median_tokens, a.median_wall_ms
        ));
    }
    s.push_str(
        "\n_Score = an independent `run_verify` (build/test) that ran AND passed after the \
         approach. Same oracle for every approach, so cross-approach deltas are method-independent. \
         Conservative lower bound: a trial where no verifier ran counts as not-passed (\"Unscored\"). \
         Prefer stable cells; small N over-states a bare %. Claims hold only for this suite._\n",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(task: &str, approach: &str, success: bool, verify_ran: bool, tokens: u64) -> TrialResult {
        TrialResult {
            task: task.into(),
            approach: approach.into(),
            trial: 0,
            success,
            verify_ran,
            pipeline_ok: true,
            tokens,
            wall_ms: tokens, // reuse for a deterministic median check
            error: None,
        }
    }

    #[test]
    fn approach_parse_roundtrips() {
        for a in [
            Approach::Solo,
            Approach::Conduct,
            Approach::ConductNoGrounding,
            Approach::ConductCrossFamily,
        ] {
            assert_eq!(Approach::parse(a.as_str()), Some(a));
        }
        assert_eq!(Approach::parse("nope"), None);
    }

    #[test]
    fn parse_approaches_dedups_and_validates() {
        let a = parse_approaches("solo, conduct ,solo").unwrap();
        assert_eq!(a, vec![Approach::Solo, Approach::Conduct]);
        assert!(parse_approaches("solo,bogus").is_err());
        assert!(parse_approaches("").is_err());
    }

    #[test]
    fn median_handles_even_and_odd() {
        assert_eq!(median(&[]), 0);
        assert_eq!(median(&[5]), 5);
        assert_eq!(median(&[3, 1, 2]), 2);
        assert_eq!(median(&[1, 2, 3, 4]), 2); // (2+3)/2 = 2 (integer)
    }

    #[test]
    fn aggregate_computes_rate_stability_and_unscored() {
        let results = vec![
            tr("t1", "conduct", true, true, 10),
            tr("t1", "conduct", true, true, 20),
            tr("t1", "conduct", false, true, 30),
            tr("t1", "solo", false, false, 0), // unscored (verify didn't run)
        ];
        let agg = aggregate(&results);

        let conduct = agg
            .per_task_approach
            .iter()
            .find(|c| c.approach == "conduct")
            .unwrap();
        assert_eq!((conduct.passed, conduct.total), (2, 3));
        assert!(!conduct.stable); // 2 pass + 1 fail
        assert_eq!(conduct.unscored, 0);
        assert_eq!(conduct.median_tokens, 20);

        let solo = agg
            .per_task_approach
            .iter()
            .find(|c| c.approach == "solo")
            .unwrap();
        assert_eq!((solo.passed, solo.total), (0, 1));
        assert!(solo.stable); // single trial, all-fail
        assert_eq!(solo.unscored, 1);

        let overall = agg
            .per_approach
            .iter()
            .find(|a| a.approach == "conduct")
            .unwrap();
        assert_eq!((overall.passed, overall.total), (2, 3));
    }

    #[test]
    fn report_serializes_with_expected_shape() {
        let results = vec![tr("t1", "solo", true, true, 5)];
        let report = SuiteReport {
            trials: 1,
            aggregate: aggregate(&results),
            results,
        };
        let json = serde_json::to_value(&report).unwrap();
        assert!(json.get("results").unwrap().is_array());
        assert!(json
            .get("aggregate")
            .unwrap()
            .get("per_approach")
            .unwrap()
            .is_array());
        assert!(json
            .get("aggregate")
            .unwrap()
            .get("per_task_approach")
            .unwrap()
            .is_array());
        let md = markdown_report(&report);
        assert!(md.contains("Pass (k/N)"));
        assert!(md.contains("solo"));
    }

    #[test]
    fn dry_run_plan_lists_matrix_without_running() {
        let tasks = vec![EvalTask {
            name: "demo".into(),
            prompt: "do it".into(),
            context: String::new(),
            verify: None,
            protected_paths: Vec::new(),
            repo_dir: PathBuf::new(),
        }];
        let plan = dry_run_plan(&tasks, &[Approach::Solo, Approach::Conduct], 3);
        assert!(plan.contains("1 task(s) × 2 approach(es) × 3 trial(s) = 6 runs"));
        assert!(plan.contains("demo"));
        assert!(plan.contains("--spend-quota"));
    }
}
