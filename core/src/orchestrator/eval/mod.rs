//! M-eval: a benchmark harness that measures whether Consilium's orchestration
//! (council + build/test grounding) actually beats a solo agent.
//!
//! Each task is run through one or more **approaches**; the harness then scores
//! the result with an **independent** `run_verify` on the produced tree. The
//! approach's own "I completed" is recorded but is NOT the score — a run that
//! reports success yet leaves a broken build scores `failed`. A trial where no
//! verifier ran is `not_run`: never a pass, and reported apart from real
//! failures. This external-oracle rule is the honesty keystone.
//!
//! The harness never touches the real quota ledger: each trial uses a fresh
//! in-memory [`QuotaStore`], so token deltas are isolated and a benchmark run
//! does not pollute `~/.consilium/usage.db`.
//!
//! Layout: this file runs trials; [`report`] is the schema-v2 report (types,
//! aggregation, markdown); [`stats`] is the statistics; [`workspace`] prepares
//! a trial's isolated repository and scores what is left in it.

mod report;
mod stats;
mod workspace;

pub use report::{
    aggregate, build_report, markdown_report, Aggregate, ApproachAggregate, CategoryAggregate,
    CellAggregate, GroupStats, ProviderTokens, ReportMeta, SuiteReport, TokenSummary, TrialResult,
    VerificationStatus, EVAL_SCHEMA_VERSION,
};
pub use stats::{summarize, MetricSummary};
pub use workspace::{prepare_trial_repo, score_tree, suite_commit, utc_timestamp, SKIPPED_DIRS};

use crate::adapters::RunRequest;
use crate::config::VerifyConfig;
use crate::event::Provider;
use crate::orchestrator::conduct::{run_conduct, ConductDeps};
use crate::orchestrator::resilience::{run_with_failover, ModelHealth, Rung};
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
    /// What kind of work the task is (`bugfix`, `parsing`, `api`, `refactor`).
    /// Required, so every result can be grouped without guessing.
    pub category: String,
    /// The fixture's language/runtime (`rust`, `python`, `node`).
    pub language: String,
    /// Rough size (`small`, `medium`).
    pub difficulty: String,
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
    /// Worker passes over the task (see [`TrialResult::attempts`]).
    attempts: u32,
    /// Model demotions during the run (see [`TrialResult::fallbacks`]).
    fallbacks: u32,
    error: Option<String>,
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
        s.push_str(&format!(
            "  - {} ({}, {}, {})\n",
            t.name, t.category, t.language, t.difficulty
        ));
    }
    s.push_str("\napproaches (rough per-trial cost):\n");
    for a in approaches {
        s.push_str(&format!("  - {} — {}\n", a.as_str(), a.call_estimate()));
    }
    s.push_str("\nSolo is cheapest; full Conduct is the most expensive. Multiply per-trial cost × trials × tasks.\n");
    s
}

/// Run the full matrix (task × approach × trial). Each trial is isolated (fresh
/// temp repo + in-memory quota). Spends real quota via `deps`. `meta` records
/// when the run started and which suite commit it used.
pub async fn run_suite(
    tasks: &[EvalTask],
    approaches: &[Approach],
    trials: u32,
    deps: &dyn EvalDeps,
    timeout: Duration,
    meta: ReportMeta,
) -> anyhow::Result<SuiteReport> {
    let mut results = Vec::new();
    for task in tasks {
        for &approach in approaches {
            for trial in 0..trials {
                let r = run_trial(task, approach, trial, deps, timeout).await?;
                eprintln!(
                    "  {} / {} / trial {} → {} ({} tok, {}C, {} ms)",
                    r.task,
                    r.approach,
                    r.trial,
                    r.verification.as_str(),
                    r.tokens.total(),
                    r.tokens.claude,
                    r.wall_ms,
                );
                results.push(r);
            }
        }
    }
    Ok(build_report(meta, trials, results))
}

/// One isolated trial: copy the starter repo, run the approach, score externally.
async fn run_trial(
    task: &EvalTask,
    approach: Approach,
    trial: u32,
    deps: &dyn EvalDeps,
    timeout: Duration,
) -> anyhow::Result<TrialResult> {
    let tmp = prepare_trial_repo(task)?;
    let cwd = tmp.path().to_path_buf();

    // Isolated ledger — never touches the real usage.db.
    let quota = QuotaStore::open_in_memory().context("opening in-memory quota store")?;

    let start = Instant::now();
    let run = run_approach(approach, task, cwd.clone(), deps, &quota, timeout).await;
    let wall_ms = start.elapsed().as_millis() as u64;

    // External, independent score — the honesty keystone. `score_tree` first
    // undoes any tampering with the protected test files. Deliberately left OUT
    // of wall_ms so the metric measures orchestration, not scoring. (NB: a
    // grounded conduct arm runs build/test once internally too, so its tree is
    // verified twice per trial — once as its gate, once here as the score.)
    let verification = score_tree(&cwd, task).await?;

    Ok(TrialResult {
        task: task.name.clone(),
        category: task.category.clone(),
        language: task.language.clone(),
        difficulty: task.difficulty.clone(),
        approach: approach.as_str().to_string(),
        trial,
        verification,
        pipeline_ok: run.pipeline_ok,
        attempts: run.attempts,
        fallbacks: run.fallbacks,
        tokens: provider_tokens(&quota)?,
        wall_ms,
        error: run.error,
    })
}

/// (worker attempts, model fallbacks) read from a conduct run's transcript:
/// every subtask lists its attempts, and the run lists its demotions.
fn conduct_effort(transcript: &serde_json::Value) -> (u32, u32) {
    let len = |value: &serde_json::Value| value.as_array().map_or(0, Vec::len) as u32;
    let attempts = transcript["subtasks"].as_array().map_or(0, |subtasks| {
        subtasks.iter().map(|s| len(&s["attempts"])).sum()
    });
    (attempts, len(&transcript["fallbacks"]))
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
        // Solo is one worker pass (none if there was no model to run it).
        let attempts = u32::from(!ladder.is_empty());
        // run_with_failover returns Ok only when a rung ran to completion.
        return match res {
            Ok(done) => ApproachRun {
                pipeline_ok: true,
                attempts,
                fallbacks: done.fallbacks.len() as u32,
                error: None,
            },
            // Every rung failed, so the ladder demoted past each one but the last.
            Err(e) => ApproachRun {
                pipeline_ok: false,
                attempts,
                fallbacks: ladder.len().saturating_sub(1) as u32,
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
        Ok(o) => {
            let (attempts, fallbacks) = conduct_effort(&o.transcript);
            ApproachRun {
                pipeline_ok: o.failed.is_none() && o.halted.is_none(),
                attempts,
                fallbacks,
                error: None,
            }
        }
        // The run aborted before producing a transcript: nothing to count.
        Err(e) => ApproachRun {
            pipeline_ok: false,
            attempts: 0,
            fallbacks: 0,
            error: Some(e.to_string()),
        },
    }
}

/// Tokens spent in a trial (input+output), split by provider, from its store.
fn provider_tokens(quota: &QuotaStore) -> anyhow::Result<ProviderTokens> {
    let sum = |p| -> anyhow::Result<u64> {
        let (i, o) = quota.totals_since(p, 0)?;
        Ok(i + o)
    };
    Ok(ProviderTokens {
        claude: sum(Provider::Claude)?,
        codex: sum(Provider::Codex)?,
        gemini: sum(Provider::Gemini)?,
        grok: sum(Provider::Grok)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn dry_run_plan_lists_matrix_without_running() {
        let tasks = vec![EvalTask {
            name: "demo".into(),
            category: "parsing".into(),
            language: "python".into(),
            difficulty: "small".into(),
            prompt: "do it".into(),
            context: String::new(),
            verify: None,
            protected_paths: Vec::new(),
            repo_dir: PathBuf::new(),
        }];
        let plan = dry_run_plan(&tasks, &[Approach::Solo, Approach::Conduct], 3);
        assert!(plan.contains("no quota spent"));
        assert!(plan.contains("1 task(s) × 2 approach(es) × 3 trial(s) = 6 runs"));
        assert!(plan.contains("demo (parsing, python, small)"));
        assert!(plan.contains("--spend-quota"));
    }

    #[test]
    fn provider_tokens_reads_all_four_providers() {
        let quota = QuotaStore::open_in_memory().unwrap();
        quota.record(Provider::Claude, 1, 1).unwrap();
        quota.record(Provider::Codex, 2, 2).unwrap();
        quota.record(Provider::Gemini, 3, 3).unwrap();
        quota.record(Provider::Grok, 4, 4).unwrap();
        assert_eq!(
            provider_tokens(&quota).unwrap(),
            ProviderTokens {
                claude: 2,
                codex: 4,
                gemini: 6,
                grok: 8,
            }
        );
    }

    /// The shape `run_conduct` writes (taken from a real transcript,
    /// 2026-11-17): attempts live under each subtask, fallbacks at the top.
    #[test]
    fn conduct_effort_counts_attempts_across_subtasks_and_fallbacks() {
        let transcript = serde_json::json!({
            "kind": "conduct",
            "subtasks": [
                { "id": 1, "attempts": [{ "attempt": 0, "decision": "rework" },
                                        { "attempt": 1, "decision": "accept" }] },
                { "id": 2, "attempts": [{ "attempt": 0, "decision": "accept" }] }
            ],
            "fallbacks": [{ "from": "codex/a", "to": "codex/b", "reason": "model unavailable" }]
        });
        assert_eq!(conduct_effort(&transcript), (3, 1));
        // a transcript without those keys counts nothing rather than failing
        assert_eq!(conduct_effort(&serde_json::json!({})), (0, 0));
    }
}
