//! Eval report schema v2: what a trial records, how trials are aggregated, and
//! how the result is rendered. Everything here is pure — no I/O, no models.
//!
//! Honesty rules the schema encodes:
//! - one explicit verification state per trial (`passed | failed | not_run`),
//!   never a pass/fail boolean that hides "the verifier did not run";
//! - every summary states its sample size, and a spread exists only for two or
//!   more samples (see [`super::stats`]);
//! - the report names the suite commit and time it was produced from.

use super::stats::{summarize, MetricSummary};
use serde::Serialize;
use std::collections::BTreeMap;

/// Version of the JSON this module writes. Bump on any breaking field change.
pub const EVAL_SCHEMA_VERSION: u32 = 2;

/// What the independent verifier did with a trial's result tree. This is THE
/// score: only `Passed` counts as a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    /// Verifier commands ran and every blocking one succeeded.
    Passed,
    /// A verifier command ran and failed.
    Failed,
    /// No verifier command resolved, so the tree could not be scored. Counted
    /// apart from real failures and never as a pass.
    NotRun,
}

impl VerificationStatus {
    /// From `run_verify`'s outcome: `ran` = a command executed, `passed` =
    /// every blocking command succeeded.
    pub fn from_verify(ran: bool, passed: bool) -> Self {
        match (ran, passed) {
            (false, _) => Self::NotRun,
            (true, true) => Self::Passed,
            (true, false) => Self::Failed,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::NotRun => "not_run",
        }
    }
}

/// Tokens spent in a trial (input + output), split by provider. Claude is the
/// expensive seat we most want to minimize: the thesis is that conduct matches
/// solo quality while spending far fewer Claude tokens by offloading the volume
/// to the other providers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ProviderTokens {
    pub claude: u64,
    pub codex: u64,
    pub gemini: u64,
    pub grok: u64,
}

impl ProviderTokens {
    pub fn total(&self) -> u64 {
        self.claude + self.codex + self.gemini + self.grok
    }
}

/// One scored trial.
#[derive(Debug, Clone, Serialize)]
pub struct TrialResult {
    pub task: String,
    pub category: String,
    pub language: String,
    pub difficulty: String,
    pub approach: String,
    pub trial: u32,
    /// THE SCORE: what an independent `run_verify` did with the result tree.
    pub verification: VerificationStatus,
    /// The approach's own reported completion (informational, not the score).
    pub pipeline_ok: bool,
    /// Worker passes over the task: 1 for solo, first tries plus reworks for
    /// conduct. Informational — a costly path, not a worse score.
    pub attempts: u32,
    /// Model demotions (a rung failed and the ladder moved on) during the trial.
    pub fallbacks: u32,
    /// Tokens spent this trial, split by provider.
    pub tokens: ProviderTokens,
    pub wall_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Token statistics over a group of trials, per provider and in total.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct TokenSummary {
    pub claude: MetricSummary,
    pub codex: MetricSummary,
    pub gemini: MetricSummary,
    pub grok: MetricSummary,
    pub total: MetricSummary,
}

/// Counts and statistics for one group of trials.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GroupStats {
    pub passed: u32,
    pub failed: u32,
    /// Trials the verifier could not score (kept distinct from real failures).
    pub not_run: u32,
    /// Sample size: `passed + failed + not_run`.
    pub total: u32,
    /// Whether every trial reached the same verification state. `None` for a
    /// single trial, which cannot agree or disagree with anything.
    pub stable: Option<bool>,
    /// Sum of worker attempts across the group.
    pub attempts: u32,
    /// Sum of model fallbacks across the group.
    pub fallbacks: u32,
    pub tokens: TokenSummary,
    pub wall_ms: MetricSummary,
}

/// Per (task, approach) summary over its N trials.
#[derive(Debug, Clone, Serialize)]
pub struct CellAggregate {
    pub task: String,
    pub approach: String,
    #[serde(flatten)]
    pub stats: GroupStats,
}

/// Per (category, approach) summary across that category's tasks.
#[derive(Debug, Clone, Serialize)]
pub struct CategoryAggregate {
    pub category: String,
    pub approach: String,
    #[serde(flatten)]
    pub stats: GroupStats,
}

/// Per-approach summary across all tasks.
#[derive(Debug, Clone, Serialize)]
pub struct ApproachAggregate {
    pub approach: String,
    #[serde(flatten)]
    pub stats: GroupStats,
}

#[derive(Debug, Clone, Serialize)]
pub struct Aggregate {
    pub per_task_approach: Vec<CellAggregate>,
    pub per_category_approach: Vec<CategoryAggregate>,
    pub per_approach: Vec<ApproachAggregate>,
}

/// Where and when a report came from — what makes it reproducible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportMeta {
    /// UTC time the run started, RFC 3339 (`2026-11-17T12:00:00Z`).
    pub generated_at: String,
    /// Commit of the repository holding the suite, `-dirty` appended when the
    /// suite had uncommitted changes, or `unknown` outside a git repository.
    pub suite_commit: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuiteReport {
    pub schema_version: u32,
    pub generated_at: String,
    pub suite_commit: String,
    /// Trials requested per (task, approach) — the N of every per-task cell.
    pub trials: u32,
    /// Runs the whole matrix calls for (tasks × approaches × trials). When
    /// `results` holds fewer, the run was stopped early and this report is
    /// partial: it covers only the trials that finished.
    pub planned_runs: u32,
    pub results: Vec<TrialResult>,
    pub aggregate: Aggregate,
}

/// Assemble a schema-v2 report from the trials scored so far. `planned_runs`
/// is the size of the whole matrix, so a report built mid-run says it is
/// partial. Pure.
pub fn build_report(
    meta: ReportMeta,
    trials: u32,
    planned_runs: u32,
    results: Vec<TrialResult>,
) -> SuiteReport {
    SuiteReport {
        schema_version: EVAL_SCHEMA_VERSION,
        generated_at: meta.generated_at,
        suite_commit: meta.suite_commit,
        trials,
        planned_runs,
        aggregate: aggregate(&results),
        results,
    }
}

fn count(trials: &[&TrialResult], status: VerificationStatus) -> u32 {
    trials.iter().filter(|r| r.verification == status).count() as u32
}

fn metric(trials: &[&TrialResult], value: impl Fn(&TrialResult) -> u64) -> MetricSummary {
    summarize(&trials.iter().map(|r| value(r)).collect::<Vec<_>>())
}

/// Counts and statistics for one non-empty group of trials.
fn group_stats(trials: &[&TrialResult]) -> GroupStats {
    let first = trials.first().map(|r| r.verification);
    GroupStats {
        passed: count(trials, VerificationStatus::Passed),
        failed: count(trials, VerificationStatus::Failed),
        not_run: count(trials, VerificationStatus::NotRun),
        total: trials.len() as u32,
        stable: (trials.len() >= 2).then(|| trials.iter().all(|r| Some(r.verification) == first)),
        attempts: trials.iter().map(|r| r.attempts).sum(),
        fallbacks: trials.iter().map(|r| r.fallbacks).sum(),
        tokens: TokenSummary {
            claude: metric(trials, |r| r.tokens.claude),
            codex: metric(trials, |r| r.tokens.codex),
            gemini: metric(trials, |r| r.tokens.gemini),
            grok: metric(trials, |r| r.tokens.grok),
            total: metric(trials, |r| r.tokens.total()),
        },
        wall_ms: metric(trials, |r| r.wall_ms),
    }
}

/// Group trials by a key, keeping groups in key order.
fn grouped<K: Ord>(
    results: &[TrialResult],
    key: impl Fn(&TrialResult) -> K,
) -> BTreeMap<K, Vec<&TrialResult>> {
    results.iter().fold(BTreeMap::new(), |mut groups, r| {
        groups.entry(key(r)).or_insert_with(Vec::new).push(r);
        groups
    })
}

/// Aggregate trial results per task, per category and per approach. Pure.
pub fn aggregate(results: &[TrialResult]) -> Aggregate {
    let per_task_approach = grouped(results, |r| (r.task.clone(), r.approach.clone()))
        .into_iter()
        .map(|((task, approach), trials)| CellAggregate {
            task,
            approach,
            stats: group_stats(&trials),
        })
        .collect();
    let per_category_approach = grouped(results, |r| (r.category.clone(), r.approach.clone()))
        .into_iter()
        .map(|((category, approach), trials)| CategoryAggregate {
            category,
            approach,
            stats: group_stats(&trials),
        })
        .collect();
    let per_approach = grouped(results, |r| r.approach.clone())
        .into_iter()
        .map(|(approach, trials)| ApproachAggregate {
            approach,
            stats: group_stats(&trials),
        })
        .collect();
    Aggregate {
        per_task_approach,
        per_category_approach,
        per_approach,
    }
}

/// A metric cell: the mean, plus `± sample std dev` when there is one.
fn metric_cell(m: &MetricSummary) -> String {
    match m.std_dev {
        Some(sd) => format!("{:.0} ± {:.0}", m.mean, sd),
        None => format!("{:.0}", m.mean),
    }
}

fn stable_cell(stable: Option<bool>) -> &'static str {
    match stable {
        None => "n/a",
        Some(true) => "yes",
        Some(false) => "**no**",
    }
}

/// The count and cost columns every table shares.
fn stats_cells(s: &GroupStats) -> String {
    format!(
        "{}/{} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {}",
        s.passed,
        s.total,
        s.failed,
        s.not_run,
        s.attempts,
        s.fallbacks,
        metric_cell(&s.tokens.claude),
        metric_cell(&s.tokens.codex),
        metric_cell(&s.tokens.gemini),
        metric_cell(&s.tokens.grok),
        metric_cell(&s.tokens.total),
        metric_cell(&s.wall_ms),
    )
}

const STATS_HEADER: &str = "Passed | Failed | Not run | Attempts | Fallbacks | Claude tok | Codex tok | Gemini tok (est.) | Grok tok | Total tok | Wall ms";
const STATS_RULE: &str = "---|---|---|---|---|---|---|---|---|---|---";

/// Render a `SuiteReport` as human-readable markdown.
pub fn markdown_report(report: &SuiteReport) -> String {
    let spread_note = if report.trials >= 2 {
        "cost figures are mean ± sample standard deviation"
    } else {
        "run-to-run spread: n/a (needs N ≥ 2)"
    };
    let finished = report.results.len();
    let coverage = if (finished as u32) < report.planned_runs {
        format!(
            "**PARTIAL** — {finished} of {} planned runs finished; the run was stopped early",
            report.planned_runs
        )
    } else {
        format!("{finished} of {} planned runs", report.planned_runs)
    };
    let mut s = format!(
        "# Consilium eval results (schema v{})\n\n\
         - Generated: {}\n\
         - Suite commit: {}\n\
         - Runs: {coverage}\n\
         - Trials per task and approach: N = {} — {}\n",
        report.schema_version, report.generated_at, report.suite_commit, report.trials, spread_note
    );

    s.push_str(&format!(
        "\n## Per approach\n\n| Approach | {STATS_HEADER} |\n|---|{STATS_RULE}|\n"
    ));
    for a in &report.aggregate.per_approach {
        s.push_str(&format!("| {} | {} |\n", a.approach, stats_cells(&a.stats)));
    }

    s.push_str(&format!(
        "\n## Per category\n\n| Category | Approach | {STATS_HEADER} |\n|---|---|{STATS_RULE}|\n"
    ));
    for c in &report.aggregate.per_category_approach {
        s.push_str(&format!(
            "| {} | {} | {} |\n",
            c.category,
            c.approach,
            stats_cells(&c.stats)
        ));
    }

    s.push_str(&format!(
        "\n## Per task\n\n| Task | Approach | Stable | {STATS_HEADER} |\n|---|---|---|{STATS_RULE}|\n"
    ));
    for c in &report.aggregate.per_task_approach {
        s.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            c.task,
            c.approach,
            stable_cell(c.stats.stable),
            stats_cells(&c.stats)
        ));
    }

    // Explain `±` only when a `±` figure is actually on the page.
    let spread_help = if s.contains('±') {
        "A `±` figure is a sample standard deviation and appears only where there are two or \
         more samples: in per-task rows it is run-to-run spread; in per-approach and \
         per-category rows it also includes differences between tasks. "
    } else {
        ""
    };
    s.push_str(&format!(
        "\n_How to read this. **Passed** = an independent `run_verify` (build/test) ran AND \
         passed on the tree the approach left behind; the approach's own \"done\" is not the \
         score. **Not run** = no verifier command resolved, so the trial could not be scored: \
         it is not a pass and not a real failure. The same oracle scores every approach, so \
         differences between approaches are not an artefact of the method. \
         **Attempts** and **Fallbacks** are sums and describe cost, not quality. \
         {spread_help}\
         Gemini runs through the Antigravity `agy` CLI, which reports no usage, so its tokens \
         are ESTIMATED (~4 characters per token) and are not directly comparable to the measured \
         Claude and Codex counts. Claims hold only for this suite at this commit and N._\n"
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(
        task: &str,
        approach: &str,
        verification: VerificationStatus,
        tokens: u64,
    ) -> TrialResult {
        TrialResult {
            task: task.into(),
            category: "bugfix".into(),
            language: "rust".into(),
            difficulty: "small".into(),
            approach: approach.into(),
            trial: 0,
            verification,
            pipeline_ok: true,
            attempts: 1,
            fallbacks: 0,
            // value goes in the Claude seat so total()==claude==tokens for asserts
            tokens: ProviderTokens {
                claude: tokens,
                ..ProviderTokens::default()
            },
            wall_ms: tokens, // reuse for a deterministic check
            error: None,
        }
    }

    #[test]
    fn verification_status_follows_the_verifier() {
        use VerificationStatus::*;
        assert_eq!(VerificationStatus::from_verify(true, true), Passed);
        assert_eq!(VerificationStatus::from_verify(true, false), Failed);
        // nothing ran: not a pass, whatever `passed` says
        assert_eq!(VerificationStatus::from_verify(false, true), NotRun);
        assert_eq!(VerificationStatus::from_verify(false, false), NotRun);
    }

    #[test]
    fn aggregate_computes_rate_stability_and_not_run() {
        use VerificationStatus::*;
        let results = vec![
            tr("t1", "conduct", Passed, 10),
            tr("t1", "conduct", Passed, 20),
            tr("t1", "conduct", Failed, 30),
            tr("t1", "solo", NotRun, 0), // the verifier did not run
        ];
        let agg = aggregate(&results);

        let conduct = &agg
            .per_task_approach
            .iter()
            .find(|c| c.approach == "conduct")
            .unwrap()
            .stats;
        assert_eq!((conduct.passed, conduct.failed, conduct.total), (2, 1, 3));
        assert_eq!(conduct.stable, Some(false)); // 2 pass + 1 fail
        assert_eq!(conduct.not_run, 0);
        assert_eq!(conduct.tokens.total.median, 20.0);
        assert_eq!(conduct.tokens.claude.mean, 20.0);
        assert_eq!(conduct.attempts, 3);

        let solo = &agg
            .per_task_approach
            .iter()
            .find(|c| c.approach == "solo")
            .unwrap()
            .stats;
        assert_eq!((solo.passed, solo.failed, solo.total), (0, 0, 1));
        assert_eq!(solo.stable, None); // a single trial cannot be stable
        assert_eq!(solo.not_run, 1);

        let overall = &agg
            .per_approach
            .iter()
            .find(|a| a.approach == "conduct")
            .unwrap()
            .stats;
        assert_eq!((overall.passed, overall.total), (2, 3));
    }

    #[test]
    fn stable_means_every_trial_reached_the_same_state() {
        use VerificationStatus::*;
        let all_failed = vec![tr("t", "solo", Failed, 1), tr("t", "solo", Failed, 2)];
        assert_eq!(
            aggregate(&all_failed).per_task_approach[0].stats.stable,
            Some(true)
        );
        // a failure and a could-not-score are different outcomes
        let mixed = vec![tr("t", "solo", Failed, 1), tr("t", "solo", NotRun, 2)];
        assert_eq!(
            aggregate(&mixed).per_task_approach[0].stats.stable,
            Some(false)
        );
    }

    #[test]
    fn report_serializes_with_expected_shape() {
        let results = vec![tr("t1", "solo", VerificationStatus::Passed, 5)];
        let report = build_report(
            ReportMeta {
                generated_at: "2026-11-17T12:00:00Z".into(),
                suite_commit: "abc".into(),
            },
            1,
            1,
            results,
        );
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["schema_version"], EVAL_SCHEMA_VERSION);
        assert!(json["results"].is_array());
        for table in ["per_approach", "per_category_approach", "per_task_approach"] {
            assert!(json["aggregate"][table].is_array(), "{table}");
        }
        // group statistics sit next to the group's key, not in a nested object
        assert_eq!(json["aggregate"]["per_approach"][0]["passed"], 1);

        let md = markdown_report(&report);
        assert!(md.contains("Passed"));
        assert!(md.contains("solo"));
        assert!(md.contains("Grok tok"));
        assert!(md.contains("Suite commit: abc"));
    }
}
