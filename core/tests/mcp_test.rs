mod common;

use common::{RecordingAdapter, ScriptedAdapter};
use consilium::config::{ModelCandidate, VerifyConfig};
use consilium::event::Provider;
use consilium::mcp::{McpServer, RunWorkerParams};
use consilium::orchestrator::council::CouncilMember;
use consilium::orchestrator::resilience::Rung;
use consilium::quota::QuotaStore;
use std::sync::{Arc, Mutex};

// ─── helpers ────────────────────────────────────────────────────────────────

fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t.com")
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

fn temp_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["commit", "--allow-empty", "-m", "init", "-q"]);
    dir
}

fn worker(label: &str, adapter: Arc<dyn consilium::adapters::Adapter>) -> CouncilMember {
    CouncilMember {
        label: label.into(),
        ladder: vec![Rung {
            candidate: ModelCandidate {
                provider: Provider::Codex,
                model: "gpt".into(),
            },
            adapter,
        }],
    }
}

fn params(worker_label: &str, cwd: &std::path::Path) -> RunWorkerParams {
    RunWorkerParams {
        prompt: "do the thing".into(),
        worker_label: worker_label.into(),
        cwd: cwd.to_string_lossy().into_owned(),
        timeout_secs: Some(30),
    }
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn run_worker_routes_writes_captures_and_uses_scoped_flags() {
    let repo = temp_repo();
    let log: Arc<Mutex<Vec<(String, bool, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let inner = ScriptedAdapter {
        pre_script: "echo hi > out.rs".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
    };
    let rec = Arc::new(RecordingAdapter::new(inner, log.clone()));
    let server = McpServer::from_parts(
        vec![worker("codex-gpt", rec)],
        None,
        QuotaStore::open_in_memory().unwrap(),
    );

    let out = server
        .run_worker_inner(params("codex-gpt", repo.path()))
        .await;

    assert!(out.ok, "expected ok; error={:?}", out.error);
    assert_eq!(out.model_used.as_deref(), Some("codex/gpt"));
    assert_eq!(out.worker_report.as_deref(), Some("did it"));
    assert!(
        out.changes.as_deref().unwrap_or("").contains("out.rs"),
        "captured changes should mention the new file; got {:?}",
        out.changes
    );
    assert!(repo.path().join("out.rs").exists());

    // Security invariant: the worker ran with advisory:false, write:true.
    let calls = log.lock().unwrap();
    assert_eq!(calls.len(), 1, "worker invoked exactly once");
    let (_, advisory, write) = &calls[0];
    assert!(
        !advisory,
        "run_worker must never relax safeguards (advisory:false)"
    );
    assert!(write, "run_worker writes are auto-approved (write:true)");
}

#[tokio::test]
async fn run_worker_unknown_label_returns_structured_error() {
    let repo = temp_repo();
    let server = McpServer::from_parts(
        vec![worker(
            "codex-gpt",
            Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "unused")),
        )],
        None,
        QuotaStore::open_in_memory().unwrap(),
    );

    let out = server
        .run_worker_inner(params("nope-missing", repo.path()))
        .await;

    assert!(!out.ok);
    let err = out.error.unwrap_or_default();
    assert!(err.contains("unknown worker_label"), "got: {err}");
    assert!(
        err.contains("codex-gpt"),
        "error should list known workers; got: {err}"
    );
}

#[tokio::test]
async fn run_worker_runs_the_configured_verifier() {
    let repo = temp_repo();
    let inner = ScriptedAdapter {
        pre_script: "echo hi > out.rs".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
    };
    let verify = VerifyConfig {
        // passes only because the worker wrote "hi" into out.rs
        test: Some("grep -q hi out.rs".into()),
        build: None,
        lint: None,
    };
    let server = McpServer::from_parts(
        vec![worker("codex-gpt", Arc::new(inner))],
        Some(verify),
        QuotaStore::open_in_memory().unwrap(),
    );

    let out = server
        .run_worker_inner(params("codex-gpt", repo.path()))
        .await;
    let v = out.verify.expect("verify should have run");
    assert!(v.ran);
    assert!(v.passed, "verify summary: {}", v.summary);
}

#[tokio::test]
async fn quota_status_reports_recorded_totals() {
    let quota = QuotaStore::open_in_memory().unwrap();
    quota.record(Provider::Gemini, 100, 20).unwrap();
    quota.record(Provider::Gemini, 50, 10).unwrap();
    quota.record(Provider::Codex, 7, 3).unwrap();
    let server = McpServer::from_parts(vec![], None, quota);

    let s = server.quota_status_inner();
    assert_eq!(s.gemini.input_tokens, 150);
    assert_eq!(s.gemini.output_tokens, 30);
    assert_eq!(s.codex.input_tokens, 7);
    assert_eq!(s.claude.input_tokens, 0);
    assert_eq!(s.window_secs, 5 * 3600);
}
