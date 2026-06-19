mod common;

use common::{RecordingAdapter, ScriptedAdapter};
use consilium::config::{ModelCandidate, VerifyConfig};
use consilium::event::Provider;
use consilium::mcp::{CouncilRunParams, McpServer, ReviewDiffParams, RunWorkerParams};
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

fn chairman_ladder(adapter: Arc<dyn consilium::adapters::Adapter>) -> Vec<Rung> {
    vec![Rung {
        candidate: ModelCandidate {
            provider: Provider::Claude,
            model: "chair".into(),
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

fn council_params(question: &str, cwd: &std::path::Path) -> CouncilRunParams {
    CouncilRunParams {
        question: question.into(),
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
        Vec::new(),
        std::path::PathBuf::from("/tmp"),
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
        Vec::new(),
        std::path::PathBuf::from("/tmp"),
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
        Vec::new(),
        std::path::PathBuf::from("/tmp"),
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
    let server = McpServer::from_parts(
        vec![],
        Vec::new(),
        Vec::new(),
        std::path::PathBuf::from("/tmp"),
        None,
        quota,
    );

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
        Vec::new(),
        std::path::PathBuf::from("/tmp"),
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
        Vec::new(),
        std::path::PathBuf::from("/tmp"),
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
    assert!(
        stdout.contains("\"council_run\""),
        "tools/list must include council_run; got:\n{stdout}"
    );
    assert!(
        stdout.contains("\"search_recall\""),
        "tools/list must include search_recall; got:\n{stdout}"
    );
    assert!(
        stdout.contains("\"page_in\""),
        "tools/list must include page_in; got:\n{stdout}"
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
        Vec::new(),
        std::path::PathBuf::from("/tmp"),
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
        Vec::new(),
        std::path::PathBuf::from("/tmp"),
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
        Vec::new(),
        std::path::PathBuf::from("/tmp"),
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
        Vec::new(),
        std::path::PathBuf::from("/tmp"),
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
        Vec::new(),
        std::path::PathBuf::from("/tmp"),
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
        Vec::new(),
        std::path::PathBuf::from("/tmp"),
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

#[tokio::test]
async fn council_run_returns_synthesis_and_answers() {
    let dir = tempfile::tempdir().unwrap();
    let server = McpServer::from_parts(
        vec![
            worker(
                "codex-gpt",
                Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "answer one")),
            ),
            worker(
                "codex-gpt-2",
                Arc::new(ScriptedAdapter::ok_with_text(
                    Provider::Codex,
                    "```json\n{\"scores\":[{\"agent\":\"A\",\"score\":8,\"justification\":\"solid\"}]}\n```",
                )),
            ),
        ],
        Vec::new(),
        chairman_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Claude,
            "combined answer",
        ))),
        std::path::PathBuf::from("/tmp"),
        None,
        QuotaStore::open_in_memory().unwrap(),
    );

    let out = server
        .council_run_inner(council_params("which option?", dir.path()))
        .await;

    assert!(out.ok, "error={:?}", out.error);
    assert!(out.synthesis.is_some(), "expected chairman synthesis");
    assert!(!out.answers.is_empty(), "expected member answers");
}

#[tokio::test]
async fn council_run_empty_question_returns_error() {
    let server = McpServer::from_parts(
        vec![],
        Vec::new(),
        Vec::new(),
        std::path::PathBuf::from("/tmp"),
        None,
        QuotaStore::open_in_memory().unwrap(),
    );
    let out = server
        .council_run_inner(council_params("   ", std::path::Path::new("/tmp")))
        .await;
    assert!(!out.ok);
    assert!(out.error.unwrap_or_default().contains("empty question"));
}

// ─── search_recall ────────────────────────────────────────────────────────────

#[test]
fn search_recall_returns_hits() {
    let base = tempfile::tempdir().unwrap();
    let store =
        consilium::orchestrator::transcript::TranscriptStore::new(base.path().to_path_buf());

    // Save some fixtures
    store
        .save(
            "task",
            &serde_json::json!({
                "id": "s1",
                "kind": "task",
                "task": "foo"
            }),
        )
        .unwrap();

    let server = McpServer::from_parts(
        vec![],
        Vec::new(),
        Vec::new(),
        base.path().to_path_buf(),
        None,
        QuotaStore::open_in_memory().unwrap(),
    );

    let out = server.search_recall_inner(consilium::mcp::SearchRecallParams {
        query: "foo".into(),
        limit: None,
    });

    assert!(out.ok, "search should succeed");
    assert_eq!(out.hits.len(), 1);
    assert_eq!(out.hits[0].id, "s1");
    assert_eq!(out.hits[0].kind, "task");
    assert_eq!(out.hits[0].task, "foo");

    let out_empty = server.search_recall_inner(consilium::mcp::SearchRecallParams {
        query: "".into(),
        limit: None,
    });

    assert!(!out_empty.ok, "empty query should fail");
}

// ─── page_in ──────────────────────────────────────────────────────────────────

#[test]
fn page_in_loads_run_by_id_and_digests_it() {
    let base = tempfile::tempdir().unwrap();
    let store =
        consilium::orchestrator::transcript::TranscriptStore::new(base.path().to_path_buf());
    store
        .save(
            "conduct",
            &serde_json::json!({
                "id": "run-x",
                "kind": "conduct",
                "task": "Do the thing",
                "summary": "all done",
                "subtasks": [{"title": "step one", "summary": "did step one"}]
            }),
        )
        .unwrap();

    let server = McpServer::from_parts(
        vec![],
        Vec::new(),
        Vec::new(),
        base.path().to_path_buf(),
        None,
        QuotaStore::open_in_memory().unwrap(),
    );

    let out = server.page_in_inner(consilium::mcp::PageInParams { id: "run-x".into() });
    assert!(out.ok, "should load; error={:?}", out.error);
    assert_eq!(out.kind.as_deref(), Some("conduct"));
    let digest = out.digest.unwrap_or_default();
    assert!(
        digest.contains("Do the thing"),
        "digest has the task: {digest}"
    );
    assert!(
        digest.contains("step one"),
        "digest has subtask title: {digest}"
    );

    let missing = server.page_in_inner(consilium::mcp::PageInParams { id: "nope".into() });
    assert!(!missing.ok && missing.error.is_some());

    let empty = server.page_in_inner(consilium::mcp::PageInParams { id: "  ".into() });
    assert!(!empty.ok);
}
