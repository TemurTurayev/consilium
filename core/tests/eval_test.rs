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
use consilium::orchestrator::eval::{self, Approach, EvalDeps};
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
    let report = eval::run_suite(&tasks, &[Approach::Solo], 1, &deps, TIMEOUT)
        .await
        .unwrap();

    let r = &report.results[0];
    assert!(r.success, "should pass external verify: {r:?}");
    assert!(r.verify_ran);
    assert!(r.pipeline_ok);
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
    let report = eval::run_suite(&tasks, &[Approach::Solo], 1, &deps, TIMEOUT)
        .await
        .unwrap();

    let r = &report.results[0];
    assert!(r.pipeline_ok, "pipeline reported completion");
    assert!(!r.success, "external verify must override pipeline_ok");
}

#[tokio::test]
async fn aggregates_over_multiple_trials() {
    let suite = make_suite("test -f good.txt");
    let tasks = eval::load_suite(suite.path(), None).unwrap();
    let deps = SoloDeps {
        pre_script: "echo ok > good.txt".into(),
    };
    let report = eval::run_suite(&tasks, &[Approach::Solo], 3, &deps, TIMEOUT)
        .await
        .unwrap();

    assert_eq!(report.results.len(), 3);
    let cell = &report.aggregate.per_task_approach[0];
    assert_eq!((cell.passed, cell.total), (3, 3));
    assert!(cell.stable);
    assert_eq!(cell.unscored, 0);
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
    let report = eval::run_suite(&tasks, &[Approach::Conduct], 1, &deps, TIMEOUT)
        .await
        .unwrap();

    let r = &report.results[0];
    assert!(
        r.success,
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
    let report = eval::run_suite(&tasks, &[Approach::ConductNoGrounding], 1, &deps, TIMEOUT)
        .await
        .unwrap();

    let r = &report.results[0];
    assert!(r.pipeline_ok, "pipeline completes");
    assert!(
        !r.success,
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
    let report = eval::run_suite(&tasks, &[Approach::Solo], 1, &deps, TIMEOUT)
        .await
        .unwrap();
    assert!(
        report.results[0].success,
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
    let report = eval::run_suite(&tasks, &[Approach::Solo], 1, &deps, TIMEOUT)
        .await
        .unwrap();
    assert!(
        !report.results[0].success,
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
        serde_json::json!({ "name": "empty", "prompt": "noop" }).to_string(),
    )
    .unwrap();

    let tasks = eval::load_suite(suite.path(), None).unwrap();
    let deps = SoloDeps {
        pre_script: String::new(),
    };
    let report = eval::run_suite(&tasks, &[Approach::Solo], 1, &deps, TIMEOUT)
        .await
        .unwrap();

    let r = &report.results[0];
    assert!(!r.verify_ran, "no command should resolve in an empty repo");
    assert!(!r.success, "a could-not-score trial counts as not-passed");
    assert_eq!(report.aggregate.per_task_approach[0].unscored, 1);
}
