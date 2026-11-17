//! M-eval harness tests — all zero quota (scripted adapters + in-memory store).
//! The LIVE benchmark matrix is operator-invoked via `consilium eval
//! --spend-quota` and is intentionally NOT exercised here.

mod common;

use common::{ScriptedAdapter, SequencedAdapter};
use consilium::adapters::Adapter;
use consilium::config::{ModelCandidate, VerifyConfig};
use consilium::event::Provider;
use consilium::orchestrator::conduct::{ConductDeps, RoleHandle};
use consilium::orchestrator::council::CouncilMember;
use consilium::orchestrator::eval::{
    self, Approach, EvalDeps, ProviderTokens, ReportMeta, SuiteReport, TrialResult,
    VerificationStatus,
};
use consilium::orchestrator::resilience::Rung;
use std::sync::Arc;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(30);

fn rung(provider: Provider, model: &str, adapter: Arc<dyn Adapter>) -> Rung {
    Rung {
        candidate: ModelCandidate {
            provider,
            model: model.into(),
        },
        adapter,
    }
}

/// A one-task suite scored by `test_cmd`, with `committed` (path, content) files
/// in the starter repo and `protected` paths restored from baseline before
/// scoring. The worker writes the scored file at runtime.
fn make_suite_full(
    test_cmd: &str,
    committed: &[(&str, &str)],
    protected: &[&str],
) -> tempfile::TempDir {
    let suite = tempfile::tempdir().unwrap();
    let repo = suite.path().join("demo").join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join(".keep"), "").unwrap();
    for (path, content) in committed {
        let full = repo.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, content).unwrap();
    }
    let manifest = serde_json::json!({
        "name": "demo",
        "category": "bugfix",
        "language": "shell",
        "difficulty": "small",
        "prompt": "do the thing",
        "verify": { "test": test_cmd },
        "protected_paths": protected,
    });
    std::fs::write(
        suite.path().join("demo").join("task.json"),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    suite
}

fn make_suite(test_cmd: &str) -> tempfile::TempDir {
    make_suite_full(test_cmd, &[], &[])
}

fn meta() -> ReportMeta {
    ReportMeta {
        generated_at: "2026-11-17T12:00:00Z".into(),
        suite_commit: "0123abc".into(),
    }
}

async fn run(
    tasks: &[eval::EvalTask],
    approaches: &[Approach],
    trials: u32,
    deps: &dyn EvalDeps,
) -> SuiteReport {
    eval::run_suite(
        tasks,
        approaches,
        trials,
        deps,
        TIMEOUT,
        meta(),
        &mut |_| {},
    )
    .await
    .unwrap()
}

/// Solo arm backed by a single scripted worker whose `pre_script` mutates the
/// trial repo. `conduct_deps` is never called for Approach::Solo.
struct SoloDeps {
    pre_script: String,
}
impl EvalDeps for SoloDeps {
    fn solo_ladder(&self) -> Vec<Rung> {
        let adapter = Arc::new(ScriptedAdapter {
            pre_script: self.pre_script.clone(),
            ..ScriptedAdapter::ok_with_text(Provider::Codex, "done")
        });
        vec![rung(Provider::Codex, "m", adapter)]
    }
    fn conduct_deps(&self, _verify: Option<VerifyConfig>, _cross_family: bool) -> ConductDeps {
        unreachable!("solo-only test deps")
    }
}

#[tokio::test]
async fn solo_passing_scores_success_and_records_tokens() {
    let suite = make_suite("test -f good.txt");
    let tasks = eval::load_suite(suite.path(), None).unwrap();
    let deps = SoloDeps {
        pre_script: "echo ok > good.txt".into(),
    };
    let report = run(&tasks, &[Approach::Solo], 1, &deps).await;

    let r = &report.results[0];
    assert_eq!(
        r.verification,
        VerificationStatus::Passed,
        "should pass external verify: {r:?}"
    );
    assert!(r.pipeline_ok);
    assert_eq!(
        (r.category.as_str(), r.language.as_str()),
        ("bugfix", "shell")
    );
    assert_eq!(r.difficulty, "small");
    assert!(
        r.tokens.total() >= 15,
        "scripted usage (10 in + 5 out) should be recorded, got {}",
        r.tokens.total()
    );
}

// KEYSTONE: the approach reports completion, but the independent verify fails →
// the trial scores FALSE. Proves the score is the external oracle, not self-report.
#[tokio::test]
async fn solo_completes_but_broken_tree_scores_fail() {
    let suite = make_suite("test -f good.txt");
    let tasks = eval::load_suite(suite.path(), None).unwrap();
    let deps = SoloDeps {
        pre_script: "echo noop > other.txt".into(), // never creates good.txt
    };
    let report = run(&tasks, &[Approach::Solo], 1, &deps).await;

    let r = &report.results[0];
    assert!(r.pipeline_ok, "pipeline reported completion");
    assert_eq!(
        r.verification,
        VerificationStatus::Failed,
        "external verify must override pipeline_ok"
    );
}

#[tokio::test]
async fn aggregates_over_multiple_trials() {
    let suite = make_suite("test -f good.txt");
    let tasks = eval::load_suite(suite.path(), None).unwrap();
    let deps = SoloDeps {
        pre_script: "echo ok > good.txt".into(),
    };
    let report = run(&tasks, &[Approach::Solo], 3, &deps).await;

    assert_eq!(report.results.len(), 3);
    let cell = &report.aggregate.per_task_approach[0];
    assert_eq!((cell.stats.passed, cell.stats.total), (3, 3));
    assert_eq!(cell.stats.stable, Some(true));
    assert_eq!(cell.stats.not_run, 0);
    assert_eq!(cell.stats.wall_ms.n, 3);
}

/// Minimal scripted conduct pipeline: conductor plans one subtask then accepts;
/// the worker runs `worker_pre`; no supervisor/reviewer/arbiter.
struct ConductTestDeps {
    worker_pre: String,
}
impl EvalDeps for ConductTestDeps {
    fn solo_ladder(&self) -> Vec<Rung> {
        unreachable!("conduct-only test deps")
    }
    fn conduct_deps(&self, verify: Option<VerifyConfig>, cross_family: bool) -> ConductDeps {
        let plan =
            r#"{"subtasks":[{"id":1,"title":"x","prompt":"write good.txt","depends_note":""}]}"#;
        let conductor = Arc::new(SequencedAdapter::new(
            Provider::Claude,
            vec![
                ScriptedAdapter::ok_with_text(Provider::Claude, plan),
                ScriptedAdapter::ok_with_text(
                    Provider::Claude,
                    r#"{"decision":"accept","feedback":""}"#,
                ),
            ],
        ));
        let worker = Arc::new(ScriptedAdapter {
            pre_script: self.worker_pre.clone(),
            ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
        });
        ConductDeps {
            conductor: RoleHandle {
                ladder: vec![rung(Provider::Claude, "m", conductor)],
            },
            workers: vec![CouncilMember {
                label: "codex".into(),
                ladder: vec![rung(Provider::Codex, "gpt", worker)],
            }],
            supervisor: None,
            reviewer: None,
            arbiter: None,
            verify,
            memory: Default::default(),
            cross_family_review: cross_family,
            max_replans: 0,
            budget: None,
        }
    }
}

#[tokio::test]
async fn conduct_approach_runs_and_scores_via_external_verify() {
    let suite = make_suite("test -f good.txt");
    let tasks = eval::load_suite(suite.path(), None).unwrap();
    let deps = ConductTestDeps {
        worker_pre: "echo ok > good.txt".into(),
    };
    let report = run(&tasks, &[Approach::Conduct], 1, &deps).await;

    let r = &report.results[0];
    assert_eq!(
        r.verification,
        VerificationStatus::Passed,
        "conduct should write good.txt and pass external verify: {r:?}"
    );
    assert!(r.pipeline_ok);
}

// A green conduct pipeline (grounding OFF) on a tree that fails the external
// oracle must still score false — the conduct-arm twin of the solo keystone.
#[tokio::test]
async fn conduct_no_grounding_green_pipeline_broken_tree_scores_fail() {
    let suite = make_suite("test -f good.txt");
    let tasks = eval::load_suite(suite.path(), None).unwrap();
    let deps = ConductTestDeps {
        worker_pre: "echo nope > other.txt".into(), // never creates good.txt
    };
    let report = run(&tasks, &[Approach::ConductNoGrounding], 1, &deps).await;

    let r = &report.results[0];
    assert!(r.pipeline_ok, "pipeline completes");
    assert_eq!(
        r.verification,
        VerificationStatus::Failed,
        "external verify overrides a green pipeline on a broken tree"
    );
}

// The verifier-cheat guard: a worker tampers the committed oracle, but the
// harness restores protected paths from baseline before scoring → grep passes.
#[tokio::test]
async fn protected_paths_restore_the_oracle_before_scoring() {
    let suite = make_suite_full(
        "grep -q canonical oracle.txt",
        &[("oracle.txt", "canonical\n")],
        &["oracle.txt"],
    );
    let tasks = eval::load_suite(suite.path(), None).unwrap();
    let deps = SoloDeps {
        pre_script: "echo HACKED > oracle.txt".into(),
    };
    let report = run(&tasks, &[Approach::Solo], 1, &deps).await;
    assert_eq!(
        report.results[0].verification,
        VerificationStatus::Passed,
        "the protected oracle should be restored before scoring"
    );
}

// Control: the SAME tamper without protection stands, so the grep fails — proves
// the restore (not luck) is what saves the protected case.
#[tokio::test]
async fn unprotected_tampered_oracle_is_scored_as_is() {
    let suite = make_suite_full(
        "grep -q canonical oracle.txt",
        &[("oracle.txt", "canonical\n")],
        &[],
    );
    let tasks = eval::load_suite(suite.path(), None).unwrap();
    let deps = SoloDeps {
        pre_script: "echo HACKED > oracle.txt".into(),
    };
    let report = run(&tasks, &[Approach::Solo], 1, &deps).await;
    assert_eq!(
        report.results[0].verification,
        VerificationStatus::Failed,
        "an unprotected tampered oracle must score fail"
    );
}

#[tokio::test]
async fn unscored_when_no_verifier_resolves() {
    // No task `verify` and an empty repo → run_verify detects nothing → ran=false.
    let suite = tempfile::tempdir().unwrap();
    let task_dir = suite.path().join("empty");
    std::fs::create_dir_all(task_dir.join("repo")).unwrap();
    std::fs::write(task_dir.join("repo").join(".keep"), "").unwrap();
    std::fs::write(
        task_dir.join("task.json"),
        serde_json::json!({
            "name": "empty", "category": "bugfix", "language": "shell",
            "difficulty": "small", "prompt": "noop"
        })
        .to_string(),
    )
    .unwrap();

    let tasks = eval::load_suite(suite.path(), None).unwrap();
    let deps = SoloDeps {
        pre_script: String::new(),
    };
    let report = run(&tasks, &[Approach::Solo], 1, &deps).await;

    let r = &report.results[0];
    assert_eq!(
        r.verification,
        VerificationStatus::NotRun,
        "no command should resolve in an empty repo"
    );
    // a could-not-score trial is neither a pass nor a real failure
    let cell = &report.aggregate.per_task_approach[0].stats;
    assert_eq!((cell.passed, cell.failed, cell.not_run), (0, 0, 1));
}

// ── Schema v2: explicit verification state, provenance, honest statistics ────

fn tokens(claude: u64, codex: u64, gemini: u64, grok: u64) -> ProviderTokens {
    ProviderTokens {
        claude,
        codex,
        gemini,
        grok,
    }
}

#[allow(clippy::too_many_arguments)]
fn trial(
    task: &str,
    category: &str,
    language: &str,
    difficulty: &str,
    verification: VerificationStatus,
    tokens: ProviderTokens,
    attempts: u32,
    fallbacks: u32,
) -> TrialResult {
    TrialResult {
        task: task.into(),
        category: category.into(),
        language: language.into(),
        difficulty: difficulty.into(),
        approach: "conduct".into(),
        trial: 0,
        verification,
        pipeline_ok: true,
        attempts,
        fallbacks,
        tokens,
        wall_ms: 1_000,
        error: None,
    }
}

fn fixture_report(results: Vec<TrialResult>) -> SuiteReport {
    let planned_runs = results.len() as u32;
    eval::build_report(meta(), 1, planned_runs, results)
}

fn find<'a>(rows: &'a serde_json::Value, key: &str, value: &str) -> &'a serde_json::Value {
    rows.as_array()
        .unwrap()
        .iter()
        .find(|row| row[key] == value)
        .unwrap_or_else(|| panic!("no row with {key}={value} in {rows}"))
}

#[test]
fn report_v2_records_verification_provenance_and_all_providers() {
    let report = fixture_report(vec![trial(
        "parse-kv",
        "parsing",
        "rust",
        "small",
        VerificationStatus::Passed,
        tokens(1, 2, 3, 4),
        2,
        1,
    )]);
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["schema_version"], 2);
    assert_eq!(json["generated_at"], "2026-11-17T12:00:00Z");
    assert_eq!(json["suite_commit"], "0123abc");
    assert_eq!(json["trials"], 1);
    let result = &json["results"][0];
    assert_eq!(result["verification"], "passed");
    assert_eq!(result["category"], "parsing");
    assert_eq!(result["language"], "rust");
    assert_eq!(result["difficulty"], "small");
    assert_eq!(result["tokens"]["grok"], 4);
    assert_eq!(result["attempts"], 2);
    assert_eq!(result["fallbacks"], 1);
    // v1's booleans are gone: one field says what the verifier did
    assert!(result.get("success").is_none() && result.get("verify_ran").is_none());
}

#[test]
fn report_v2_spells_out_every_verification_state() {
    for (status, word) in [
        (VerificationStatus::Passed, "passed"),
        (VerificationStatus::Failed, "failed"),
        (VerificationStatus::NotRun, "not_run"),
    ] {
        let report = fixture_report(vec![trial(
            "t",
            "bugfix",
            "rust",
            "small",
            status,
            tokens(0, 0, 0, 0),
            1,
            0,
        )]);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["results"][0]["verification"], word);
    }
}

#[test]
fn report_v2_counts_states_apart_and_groups_by_category() {
    let none = tokens(0, 0, 0, 0);
    let report = fixture_report(vec![
        trial(
            "parse-kv",
            "parsing",
            "rust",
            "small",
            VerificationStatus::Passed,
            none,
            1,
            0,
        ),
        trial(
            "parse-duration",
            "parsing",
            "rust",
            "medium",
            VerificationStatus::Failed,
            none,
            3,
            0,
        ),
        trial(
            "fix-sum",
            "bugfix",
            "rust",
            "small",
            VerificationStatus::NotRun,
            none,
            1,
            2,
        ),
    ]);
    let json = serde_json::to_value(&report).unwrap();
    let aggregate = &json["aggregate"];

    let parsing = find(&aggregate["per_category_approach"], "category", "parsing");
    assert_eq!(parsing["approach"], "conduct");
    assert_eq!(
        (
            &parsing["passed"],
            &parsing["failed"],
            &parsing["not_run"],
            &parsing["total"]
        ),
        (&1.into(), &1.into(), &0.into(), &2.into())
    );
    let bugfix = find(&aggregate["per_category_approach"], "category", "bugfix");
    assert_eq!(
        (
            &bugfix["passed"],
            &bugfix["failed"],
            &bugfix["not_run"],
            &bugfix["total"]
        ),
        (&0.into(), &0.into(), &1.into(), &1.into())
    );

    let overall = find(&aggregate["per_approach"], "approach", "conduct");
    assert_eq!(
        (
            &overall["passed"],
            &overall["failed"],
            &overall["not_run"],
            &overall["total"]
        ),
        (&1.into(), &1.into(), &1.into(), &3.into())
    );
    // attempts and fallbacks are summed, so a costly path is visible
    assert_eq!(overall["attempts"], 5);
    assert_eq!(overall["fallbacks"], 2);
}

#[test]
fn report_v2_counts_grok_in_the_token_totals() {
    assert_eq!(tokens(1, 2, 3, 4).total(), 10);
    let report = fixture_report(vec![trial(
        "t",
        "api",
        "node",
        "small",
        VerificationStatus::Passed,
        tokens(1, 2, 3, 4),
        1,
        0,
    )]);
    let json = serde_json::to_value(&report).unwrap();
    let overall = find(&json["aggregate"]["per_approach"], "approach", "conduct");
    assert_eq!(overall["tokens"]["grok"]["mean"], 4.0);
    assert_eq!(overall["tokens"]["total"]["mean"], 10.0);
}

#[test]
fn report_v2_states_sample_size_and_gives_no_spread_for_one_trial() {
    let report = fixture_report(vec![trial(
        "t",
        "api",
        "node",
        "small",
        VerificationStatus::Passed,
        tokens(100, 0, 0, 0),
        1,
        0,
    )]);
    let json = serde_json::to_value(&report).unwrap();
    let cell = find(&json["aggregate"]["per_task_approach"], "task", "t");
    assert_eq!(cell["wall_ms"]["n"], 1);
    assert_eq!(cell["tokens"]["claude"]["n"], 1);
    assert!(
        cell["wall_ms"]["std_dev"].is_null(),
        "no spread from one sample"
    );
    assert!(cell["tokens"]["claude"]["std_dev"].is_null());
    assert!(cell["stable"].is_null(), "one trial cannot be 'stable'");

    let markdown = eval::markdown_report(&report);
    assert!(markdown.contains("schema v2"), "{markdown}");
    assert!(markdown.contains("N = 1"), "{markdown}");
    assert!(
        markdown.contains("n/a"),
        "spread must read n/a at N = 1: {markdown}"
    );
    assert!(!markdown.contains('±'), "no ± figure at N = 1: {markdown}");
}

#[test]
fn report_v2_gives_mean_and_sample_spread_from_two_trials() {
    let slow = TrialResult {
        trial: 1,
        wall_ms: 3_000,
        ..trial(
            "t",
            "api",
            "node",
            "small",
            VerificationStatus::Failed,
            tokens(300, 0, 0, 0),
            1,
            0,
        )
    };
    let fast = trial(
        "t",
        "api",
        "node",
        "small",
        VerificationStatus::Passed,
        tokens(100, 0, 0, 0),
        1,
        0,
    );
    let report = eval::build_report(meta(), 2, 2, vec![fast, slow]);
    let json = serde_json::to_value(&report).unwrap();
    let cell = find(&json["aggregate"]["per_task_approach"], "task", "t");
    assert_eq!(cell["tokens"]["claude"]["n"], 2);
    assert_eq!(cell["tokens"]["claude"]["mean"], 200.0);
    assert_eq!(cell["tokens"]["claude"]["median"], 200.0);
    let spread = cell["tokens"]["claude"]["std_dev"].as_f64().unwrap();
    assert!(
        (spread - 141.421).abs() < 0.01,
        "sample std dev, got {spread}"
    );
    assert_eq!(cell["stable"], false, "one pass and one fail disagree");

    let markdown = eval::markdown_report(&report);
    assert!(markdown.contains("N = 2"), "{markdown}");
    assert!(markdown.contains('±'), "{markdown}");
}

#[test]
fn load_suite_rejects_a_manifest_without_category_language_difficulty() {
    let suite = tempfile::tempdir().unwrap();
    let task_dir = suite.path().join("bare");
    std::fs::create_dir_all(task_dir.join("repo")).unwrap();
    std::fs::write(
        task_dir.join("task.json"),
        serde_json::json!({ "name": "bare", "prompt": "noop" }).to_string(),
    )
    .unwrap();
    let error = format!("{:#}", eval::load_suite(suite.path(), None).unwrap_err());
    assert!(error.contains("category"), "got: {error}");
    assert!(error.contains("task.json"), "got: {error}");
}

// ── Progress: a run that is stopped early keeps what it has measured ─────────

#[tokio::test]
async fn every_finished_trial_is_reported_as_a_complete_partial_report() {
    let suite = make_suite("test -f good.txt");
    let tasks = eval::load_suite(suite.path(), None).unwrap();
    let deps = SoloDeps {
        pre_script: "echo ok > good.txt".into(),
    };

    // (trials finished, runs planned, passes counted) at each report
    let mut seen: Vec<(usize, u32, u32)> = Vec::new();
    let mut on_progress = |partial: &SuiteReport| {
        seen.push((
            partial.results.len(),
            partial.planned_runs,
            partial.aggregate.per_approach[0].stats.passed,
        ));
    };
    let report = eval::run_suite(
        &tasks,
        &[Approach::Solo],
        3,
        &deps,
        TIMEOUT,
        meta(),
        &mut on_progress,
    )
    .await
    .unwrap();

    // one report per finished trial, each already aggregated
    assert_eq!(seen, vec![(1, 3, 1), (2, 3, 2), (3, 3, 3)]);
    assert_eq!((report.results.len(), report.planned_runs), (3, 3));
}

#[test]
fn report_says_how_many_planned_runs_it_covers() {
    let none = tokens(0, 0, 0, 0);
    let one = trial(
        "t",
        "api",
        "node",
        "small",
        VerificationStatus::Passed,
        none,
        1,
        0,
    );

    // 1 of 24 planned runs: a run that was stopped early
    let partial = eval::build_report(meta(), 1, 24, vec![one.clone()]);
    let json = serde_json::to_value(&partial).unwrap();
    assert_eq!(json["planned_runs"], 24);
    let markdown = eval::markdown_report(&partial);
    assert!(markdown.contains("PARTIAL"), "{markdown}");
    assert!(markdown.contains("1 of 24"), "{markdown}");

    let complete = eval::build_report(meta(), 1, 1, vec![one]);
    let markdown = eval::markdown_report(&complete);
    assert!(!markdown.contains("PARTIAL"), "{markdown}");
    assert!(markdown.contains("1 of 1"), "{markdown}");
}

#[test]
fn save_report_replaces_the_file_whole_and_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("results").join("run.json");
    let none = tokens(0, 0, 0, 0);
    let first = trial(
        "a",
        "api",
        "node",
        "small",
        VerificationStatus::Passed,
        none,
        1,
        0,
    );
    let second = trial(
        "b",
        "api",
        "node",
        "small",
        VerificationStatus::Failed,
        none,
        1,
        0,
    );

    // the directory is created; the file is valid JSON after every save
    eval::save_report(
        &path,
        &eval::build_report(meta(), 1, 2, vec![first.clone()]),
    )
    .unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["results"].as_array().unwrap().len(), 1);

    eval::save_report(
        &path,
        &eval::build_report(meta(), 1, 2, vec![first, second]),
    )
    .unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["results"].as_array().unwrap().len(), 2);
    assert_eq!(saved["schema_version"], 2);

    // no temporary file is left next to it
    let names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["run.json"]);
}

// ── Attempts and fallbacks: recorded, never part of the score ────────────────

/// Solo ladder whose first model is refused (real codex wording), so the run
/// lands on the second rung.
struct SoloWithDeadFirstRung;
impl EvalDeps for SoloWithDeadFirstRung {
    fn solo_ladder(&self) -> Vec<Rung> {
        let dead = Arc::new(ScriptedAdapter::failing(
            Provider::Codex,
            "The 'gpt-x' model is not supported when using Codex with a ChatGPT account.",
        ));
        let alive = Arc::new(ScriptedAdapter {
            pre_script: "echo ok > good.txt".into(),
            ..ScriptedAdapter::ok_with_text(Provider::Codex, "done")
        });
        vec![
            rung(Provider::Codex, "gpt-x", dead),
            rung(Provider::Codex, "gpt-y", alive),
        ]
    }
    fn conduct_deps(&self, _verify: Option<VerifyConfig>, _cross_family: bool) -> ConductDeps {
        unreachable!("solo-only test deps")
    }
}

#[tokio::test]
async fn solo_records_one_attempt_and_each_model_fallback() {
    let suite = make_suite("test -f good.txt");
    let tasks = eval::load_suite(suite.path(), None).unwrap();
    let report = run(&tasks, &[Approach::Solo], 1, &SoloWithDeadFirstRung).await;
    let r = &report.results[0];
    assert_eq!(r.verification, VerificationStatus::Passed, "{r:?}");
    assert_eq!(r.attempts, 1, "solo is a single worker pass");
    assert_eq!(r.fallbacks, 1, "one demotion: gpt-x → gpt-y");
}

/// Conduct pipeline whose conductor sends the worker back once before accepting.
struct ConductWithOneRework;
impl EvalDeps for ConductWithOneRework {
    fn solo_ladder(&self) -> Vec<Rung> {
        unreachable!("conduct-only test deps")
    }
    fn conduct_deps(&self, verify: Option<VerifyConfig>, cross_family: bool) -> ConductDeps {
        let plan =
            r#"{"subtasks":[{"id":1,"title":"x","prompt":"write good.txt","depends_note":""}]}"#;
        let conductor = Arc::new(SequencedAdapter::new(
            Provider::Claude,
            vec![
                ScriptedAdapter::ok_with_text(Provider::Claude, plan),
                ScriptedAdapter::ok_with_text(
                    Provider::Claude,
                    r#"{"decision":"rework","feedback":"try again"}"#,
                ),
                ScriptedAdapter::ok_with_text(
                    Provider::Claude,
                    r#"{"decision":"accept","feedback":""}"#,
                ),
            ],
        ));
        let worker = Arc::new(ScriptedAdapter {
            pre_script: "echo ok > good.txt".into(),
            ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
        });
        ConductDeps {
            conductor: RoleHandle {
                ladder: vec![rung(Provider::Claude, "m", conductor)],
            },
            workers: vec![CouncilMember {
                label: "codex".into(),
                ladder: vec![rung(Provider::Codex, "gpt", worker)],
            }],
            supervisor: None,
            reviewer: None,
            arbiter: None,
            verify,
            memory: Default::default(),
            cross_family_review: cross_family,
            max_replans: 0,
            budget: None,
        }
    }
}

#[tokio::test]
async fn conduct_records_every_worker_attempt_including_rework() {
    let suite = make_suite("test -f good.txt");
    let tasks = eval::load_suite(suite.path(), None).unwrap();
    // grounding off, so the scripted "rework" verdict is what drives attempt 2
    let report = run(
        &tasks,
        &[Approach::ConductNoGrounding],
        1,
        &ConductWithOneRework,
    )
    .await;
    let r = &report.results[0];
    assert_eq!(r.verification, VerificationStatus::Passed, "{r:?}");
    assert_eq!(r.attempts, 2, "first attempt + one rework");
    assert_eq!(r.fallbacks, 0);
}

// ── The quota gate: a bare `consilium eval` plans and spends nothing ─────────

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("core/ has a parent")
        .to_path_buf()
}

fn files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .flat_map(|path| {
            if path.is_dir() {
                files_under(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}

#[test]
fn bare_eval_is_a_dry_run_that_spends_and_writes_nothing() {
    let cwd = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    // A config that cannot be parsed: a dry run must not even open it.
    std::fs::write(cwd.path().join("consilium.config.json"), "{ not json").unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_consilium"))
        .arg("eval")
        .arg("--suite")
        .arg(repo_root().join("eval/tasks"))
        .current_dir(cwd.path())
        .env("HOME", home.path())
        // No provider CLI can be found, let alone run.
        .env("PATH", "/nonexistent")
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("no quota spent"), "{stdout}");
    assert!(stdout.contains("parse-kv"), "tasks are listed: {stdout}");
    assert!(
        stdout.contains("solo") && stdout.contains("conduct"),
        "{stdout}"
    );
    assert!(
        stdout.contains("model call"),
        "call estimates are shown: {stdout}"
    );
    // nothing written: no results file, no usage ledger, no runs directory
    assert!(!cwd.path().join("eval").exists(), "a dry run wrote results");
    assert_eq!(files_under(home.path()), Vec::<std::path::PathBuf>::new());
}
