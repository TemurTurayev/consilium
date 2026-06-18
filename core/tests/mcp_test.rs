mod common;

use common::{RecordingAdapter, ScriptedAdapter};
use consilium::config::{ModelCandidate, VerifyConfig};
use consilium::event::Provider;
use consilium::mcp::{McpServer, ReviewDiffParams, RunWorkerParams};
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

fn reviewer_ladder(adapter: Arc<dyn consilium::adapters::Adapter>) -> Vec<Rung> {
    vec![Rung {
        candidate: ModelCandidate {
            provider: Provider::Gemini,
            model: "g".into(),
        },
        adapter,
    }]
}

fn review_params(diff: &str, cwd: &std::path::Path) -> ReviewDiffParams {
    ReviewDiffParams {
        diff: diff.into(),
        cwd: Some(cwd.to_string_lossy().into_owned()),
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
        Vec::new(),
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
        Vec::new(),
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
        Vec::new(),
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
    let server = McpServer::from_parts(vec![], Vec::new(), None, quota);

    let s = server.quota_status_inner();
    assert_eq!(s.gemini.input_tokens, 150);
    assert_eq!(s.gemini.output_tokens, 30);
    assert_eq!(s.codex.input_tokens, 7);
    assert_eq!(s.claude.input_tokens, 0);
    assert_eq!(s.window_secs, 5 * 3600);
}

#[tokio::test]
async fn run_worker_all_rungs_fail_returns_structured_error() {
    let repo = temp_repo();
    let server = McpServer::from_parts(
        vec![worker(
            "codex-gpt",
            Arc::new(ScriptedAdapter::failing(Provider::Codex, "model exploded")),
        )],
        Vec::new(),
        None,
        QuotaStore::open_in_memory().unwrap(),
    );

    let out = server
        .run_worker_inner(params("codex-gpt", repo.path()))
        .await;

    assert!(!out.ok, "all rungs failed → ok:false");
    assert!(out.error.is_some(), "should carry a structured error");
    assert!(out.model_used.is_none());
}

#[tokio::test]
async fn run_worker_non_git_cwd_degrades_changes_to_none() {
    // capture_changes errors in a non-git dir; the tool degrades to changes:None
    // (best-effort) rather than failing the worker — the conductor still gets ok.
    let dir = tempfile::tempdir().unwrap(); // deliberately NOT a git repo
    let server = McpServer::from_parts(
        vec![worker(
            "codex-gpt",
            Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "did it")),
        )],
        Vec::new(),
        None,
        QuotaStore::open_in_memory().unwrap(),
    );

    let out = server
        .run_worker_inner(params("codex-gpt", dir.path()))
        .await;

    assert!(out.ok, "worker succeeded; error={:?}", out.error);
    assert!(
        out.changes.is_none(),
        "non-git cwd → capture_changes degrades to None, got {:?}",
        out.changes
    );
}

// Real stdio-transport smoke: spawn the `consilium mcp` binary, drive the MCP
// handshake, and assert tools/list returns all tools — protects the rmcp serve
// wiring (the inner-method tests bypass the transport). Isolated via a temp HOME
// (the quota db) and a temp cwd (the config), so it touches nothing real.
#[test]
fn mcp_stdio_server_lists_all_tools() {
    use std::io::Write;

    let home = tempfile::tempdir().unwrap();
    let proj = tempfile::tempdir().unwrap();
    std::fs::write(
        proj.path().join("consilium.config.json"),
        consilium::config::Config::default()
            .to_pretty_json()
            .unwrap(),
    )
    .unwrap();

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_consilium"))
        .arg("mcp")
        .current_dir(proj.path())
        .env("HOME", home.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let reqs = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        "\n",
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(reqs.as_bytes())
        .unwrap();
    // stdin dropped above → EOF → the server responds, then exits cleanly.

    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"run_worker\""),
        "tools/list must include run_worker; got:\n{stdout}"
    );
    assert!(
        stdout.contains("\"quota_status\""),
        "tools/list must include quota_status; got:\n{stdout}"
    );
    assert!(
        stdout.contains("\"review_diff\""),
        "tools/list must include review_diff; got:\n{stdout}"
    );
}

// ─── review_diff ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn review_diff_parses_verdict_and_flags_critical() {
    let dir = tempfile::tempdir().unwrap();
    let verdict = r#"{"findings":[{"severity":"critical","file":"a.rs","description":"oops"}]}"#;
    let server = McpServer::from_parts(
        vec![],
        reviewer_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Gemini,
            verdict,
        ))),
        None,
        QuotaStore::open_in_memory().unwrap(),
    );

    let out = server
        .review_diff_inner(review_params("diff --git a b", dir.path()))
        .await;

    assert!(out.ok, "error={:?}", out.error);
    assert!(out.parse_ok);
    assert!(out.has_critical, "a critical finding must set has_critical");
    assert_eq!(out.findings.len(), 1);
    assert_eq!(out.findings[0].severity, "critical");
    assert_eq!(out.findings[0].file, "a.rs");
    assert_eq!(
        out.model_used.as_deref(),
        Some("gemini/g"),
        "the reviewing model should be reported (cross-family signal)"
    );
}

// The blocking/non-blocking boundary: an `important`-only verdict is NOT critical
// but its findings must still surface to the conductor.
#[tokio::test]
async fn review_diff_important_only_is_not_critical_but_findings_surface() {
    let dir = tempfile::tempdir().unwrap();
    let verdict = r#"{"findings":[{"severity":"important","file":"a.rs","description":"x"}]}"#;
    let server = McpServer::from_parts(
        vec![],
        reviewer_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Gemini,
            verdict,
        ))),
        None,
        QuotaStore::open_in_memory().unwrap(),
    );

    let out = server
        .review_diff_inner(review_params("some diff", dir.path()))
        .await;

    assert!(out.ok && out.parse_ok);
    assert!(!out.has_critical, "important is not blocking");
    assert_eq!(
        out.findings.len(),
        1,
        "non-critical findings must still surface"
    );
    assert_eq!(out.findings[0].severity, "important");
}

#[tokio::test]
async fn review_diff_clean_verdict_has_no_findings() {
    let dir = tempfile::tempdir().unwrap();
    let server = McpServer::from_parts(
        vec![],
        reviewer_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Gemini,
            r#"{"findings":[]}"#,
        ))),
        None,
        QuotaStore::open_in_memory().unwrap(),
    );

    let out = server
        .review_diff_inner(review_params("some diff", dir.path()))
        .await;

    assert!(out.ok && out.parse_ok);
    assert!(!out.has_critical);
    assert!(out.findings.is_empty());
}

// An unparseable review must fail CLOSED: parse_ok=false (the conductor treats it
// as unusable), and the raw text is surfaced for inspection.
#[tokio::test]
async fn review_diff_unparseable_output_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let server = McpServer::from_parts(
        vec![],
        reviewer_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Gemini,
            "looks fine to me, shipping it",
        ))),
        None,
        QuotaStore::open_in_memory().unwrap(),
    );

    let out = server
        .review_diff_inner(review_params("some diff", dir.path()))
        .await;

    assert!(out.ok, "the reviewer ran");
    assert!(
        !out.parse_ok,
        "non-JSON output → parse_ok:false (fail closed)"
    );
    assert!(!out.has_critical);
    assert!(out.raw_review.is_some(), "raw text surfaced for inspection");
}

#[tokio::test]
async fn review_diff_empty_diff_returns_error() {
    let server = McpServer::from_parts(
        vec![],
        Vec::new(),
        None,
        QuotaStore::open_in_memory().unwrap(),
    );

    let out = server
        .review_diff_inner(review_params("   ", std::path::Path::new("/tmp")))
        .await;

    assert!(!out.ok);
    assert!(out.error.unwrap_or_default().contains("empty diff"));
}

#[tokio::test]
async fn review_diff_all_rungs_fail_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let server = McpServer::from_parts(
        vec![],
        reviewer_ladder(Arc::new(ScriptedAdapter::failing(
            Provider::Gemini,
            "reviewer down",
        ))),
        None,
        QuotaStore::open_in_memory().unwrap(),
    );

    let out = server
        .review_diff_inner(review_params("some diff", dir.path()))
        .await;

    assert!(!out.ok, "all reviewer rungs failed → ok:false");
    assert!(out.error.is_some());
    assert!(!out.parse_ok);
}
