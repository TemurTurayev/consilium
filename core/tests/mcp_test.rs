mod common;

use common::{RecordingAdapter, ScriptedAdapter};
use consilium::config::{ModelCandidate, VerifyConfig};
use consilium::event::Provider;
use consilium::mcp::{
    CouncilRunParams, McpServer, McpServerDeps, ReviewDiffParams, RunWorkerParams,
};
use consilium::orchestrator::council::CouncilMember;
use consilium::orchestrator::resilience::Rung;
use consilium::quota::QuotaStore;
use std::sync::{Arc, Mutex};

// ─── helpers ────────────────────────────────────────────────────────────────

/// (prompt, advisory, write) entries recorded per adapter invocation.
type CallLog = Arc<Mutex<Vec<(String, bool, bool)>>>;

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
    // Hermetic identity: bare CI runners have no global user.name/email.
    git(dir.path(), &["config", "user.name", "consilium-test"]);
    git(
        dir.path(),
        &["config", "user.email", "test@consilium.local"],
    );
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
        author_model: None,
        task: None,
        cross_examine: None,
    }
}

fn council_params(question: &str, cwd: &std::path::Path) -> CouncilRunParams {
    CouncilRunParams {
        question: question.into(),
        cwd: Some(cwd.to_string_lossy().into_owned()),
        timeout_secs: Some(30),
    }
}

/// A throwaway transcript directory. The tools save their runs, so no test
/// may point them at a shared place.
fn scratch_transcripts() -> std::path::PathBuf {
    tempfile::tempdir().unwrap().keep()
}

fn rung(provider: Provider, model: &str, adapter: Arc<dyn consilium::adapters::Adapter>) -> Rung {
    Rung {
        candidate: ModelCandidate {
            provider,
            model: model.into(),
        },
        adapter,
    }
}

fn recall(server: &McpServer, query: &str) -> Vec<consilium::orchestrator::transcript::SearchHit> {
    server
        .search_recall_inner(consilium::mcp::SearchRecallParams {
            query: query.into(),
            limit: None,
        })
        .hits
}

fn digest_of(server: &McpServer, id: &str) -> String {
    server
        .page_in_inner(consilium::mcp::PageInParams { id: id.into() })
        .digest
        .unwrap_or_default()
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// `run_worker` wants an exact label, and the conductor had no way to learn
/// one short of provoking the "unknown worker_label" error.
#[tokio::test]
async fn lineup_names_the_workers_run_worker_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let ok = |provider| Arc::new(ScriptedAdapter::ok_with_text(provider, "unused"));
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![
            CouncilMember {
                label: "gemini-claude-opus-5-5-high".into(),
                ladder: vec![
                    rung(
                        Provider::Gemini,
                        "claude-opus-5-5-high",
                        ok(Provider::Gemini),
                    ),
                    rung(Provider::Claude, "claude-opus-5-5", ok(Provider::Claude)),
                ],
            },
            worker("codex-gpt", ok(Provider::Codex)),
        ],
        reviewer: reviewer_ladder(ok(Provider::Gemini)),
        chairman: chairman_ladder(ok(Provider::Claude)),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(dir.path().to_path_buf())
    .with_config_source(Some("/home/u/.consilium/consilium.config.json".into()));

    let out = server.lineup_inner();

    let labels: Vec<&str> = out.workers.iter().map(|w| w.label.as_str()).collect();
    assert_eq!(labels, ["gemini-claude-opus-5-5-high", "codex-gpt"]);
    assert_eq!(
        out.workers[0].models,
        ["gemini/claude-opus-5-5-high", "claude/claude-opus-5-5"]
    );
    assert_eq!(out.chairman, ["claude/chair"]);
    assert_eq!(out.reviewer, ["gemini/g"]);
    assert_eq!(
        out.config.as_deref(),
        Some("/home/u/.consilium/consilium.config.json")
    );
    assert_eq!(out.launch_root, dir.path().to_string_lossy());
}

/// Attached-mode runs were never saved, so `search_recall` and `page_in` —
/// tools of this same server — could not recall anything it had done.
#[tokio::test]
async fn council_run_is_saved_where_recall_finds_it() {
    let dir = tempfile::tempdir().unwrap();
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![worker(
            "codex-gpt",
            Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "answer one")),
        )],
        reviewer: Vec::new(),
        chairman: chairman_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Claude,
            "combined answer",
        ))),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(dir.path().to_path_buf());

    let out = server
        .council_run_inner(council_params("which hyperdrive option?", dir.path()))
        .await;

    let id = out.transcript_id.expect("the run is saved");
    let hits = recall(&server, "hyperdrive");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, id);
    assert_eq!(hits[0].kind, "council");

    let digest = digest_of(&server, &id);
    assert!(digest.contains("which hyperdrive option?"), "{digest}");
    assert!(digest.contains("combined answer"), "{digest}");
    assert!(digest.contains("codex-gpt"), "{digest}");
}

/// A council that only got its answer after a member fell back is a degraded
/// council; the conductor must be able to see that.
#[tokio::test]
async fn council_run_reports_the_fallbacks_it_took() {
    let dir = tempfile::tempdir().unwrap();
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![CouncilMember {
            label: "codex-a".into(),
            ladder: vec![
                rung(
                    Provider::Codex,
                    "a",
                    Arc::new(ScriptedAdapter::failing(
                        Provider::Codex,
                        "upstream exploded",
                    )),
                ),
                rung(
                    Provider::Codex,
                    "b",
                    Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "answer")),
                ),
            ],
        }],
        reviewer: Vec::new(),
        chairman: chairman_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Claude,
            "combined answer",
        ))),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(dir.path().to_path_buf());

    let out = server
        .council_run_inner(council_params("which option?", dir.path()))
        .await;

    assert!(out.ok, "error={:?}", out.error);
    assert!(
        out.fallbacks.iter().any(|f| f.contains("codex/a")
            && f.contains("codex/b")
            && f.contains("upstream exploded")),
        "{:?}",
        out.fallbacks
    );
}

#[tokio::test]
async fn review_diff_is_saved_where_recall_finds_it() {
    let repo = temp_repo();
    let verdict = r#"```json
{"findings":[{"severity":"important","file":"src/parser.rs","description":"the tokenizer drops the final newline"}]}
```"#;
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![],
        reviewer: reviewer_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Gemini,
            verdict,
        ))),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(repo.path().to_path_buf());

    let out = server
        .review_diff_inner(review_params(
            "diff --git a/src/parser.rs b/src/parser.rs\n+x",
            repo.path(),
        ))
        .await;

    let id = out.transcript_id.expect("the review is saved");
    let hits = recall(&server, "tokenizer");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, id);
    assert_eq!(hits[0].kind, "review");
    let digest = digest_of(&server, &id);
    assert!(digest.contains("tokenizer"), "{digest}");
    assert!(digest.contains("gemini/g"), "who reviewed: {digest}");
}

#[tokio::test]
async fn run_worker_is_saved_where_recall_finds_it() {
    let repo = temp_repo();
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![worker(
            "codex-gpt",
            Arc::new(ScriptedAdapter::ok_with_text(
                Provider::Codex,
                "added the flux capacitor",
            )),
        )],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(repo.path().to_path_buf());

    let out = server
        .run_worker_inner(params("codex-gpt", repo.path()))
        .await;

    assert!(out.ok, "error={:?}", out.error);
    let id = out.transcript_id.expect("the run is saved");
    // found by the task it was given and by what the worker reported
    assert_eq!(recall(&server, "do the thing")[0].id, id);
    let hits = recall(&server, "flux capacitor");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, "worker");
    let digest = digest_of(&server, &id);
    assert!(digest.contains("codex-gpt"), "{digest}");
    assert!(digest.contains("codex/gpt"), "which model ran: {digest}");
}

#[tokio::test]
async fn run_worker_routes_writes_captures_and_uses_scoped_flags() {
    let repo = temp_repo();
    let log: CallLog = Arc::new(Mutex::new(Vec::new()));
    let inner = ScriptedAdapter {
        pre_script: "echo hi > out.rs".into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
    };
    let rec = Arc::new(RecordingAdapter::new(inner, log.clone()));
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![worker("codex-gpt", rec)],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(repo.path().to_path_buf());

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
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![worker(
            "codex-gpt",
            Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "unused")),
        )],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(repo.path().to_path_buf());

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
        timeout_secs: None,
    };
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![worker("codex-gpt", Arc::new(inner))],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: Some(verify),
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(repo.path().to_path_buf());

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
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota,
    });

    let s = server.quota_status_inner();
    assert_eq!(s.gemini.input_tokens, 150);
    assert_eq!(s.gemini.output_tokens, 30);
    assert_eq!(s.codex.input_tokens, 7);
    assert_eq!(s.claude.input_tokens, 0);
    assert_eq!(s.grok.input_tokens, 0);
    assert_eq!(s.window_secs, 5 * 3600);
}

#[tokio::test]
async fn run_worker_all_rungs_fail_returns_structured_error() {
    let repo = temp_repo();
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![worker(
            "codex-gpt",
            Arc::new(ScriptedAdapter::failing(Provider::Codex, "model exploded")),
        )],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(repo.path().to_path_buf());

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
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![worker(
            "codex-gpt",
            Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "did it")),
        )],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(dir.path().to_path_buf());

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
    assert!(
        stdout.contains("\"lineup\""),
        "tools/list must include lineup; got:\n{stdout}"
    );
}

// ─── review_diff ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn review_diff_parses_verdict_and_flags_critical() {
    let dir = tempfile::tempdir().unwrap();
    let verdict = r#"{"findings":[{"severity":"critical","file":"a.rs","description":"oops"}]}"#;
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![],
        reviewer: reviewer_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Gemini,
            verdict,
        ))),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(dir.path().to_path_buf());

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
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![],
        reviewer: reviewer_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Gemini,
            verdict,
        ))),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(dir.path().to_path_buf());

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
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![],
        reviewer: reviewer_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Gemini,
            r#"{"findings":[]}"#,
        ))),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(dir.path().to_path_buf());

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
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![],
        reviewer: reviewer_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Gemini,
            "looks fine to me, shipping it",
        ))),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(dir.path().to_path_buf());

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
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    });

    let out = server
        .review_diff_inner(review_params("   ", std::path::Path::new("/tmp")))
        .await;

    assert!(!out.ok);
    assert!(out.error.unwrap_or_default().contains("empty diff"));
}

#[tokio::test]
async fn review_diff_all_rungs_fail_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![],
        reviewer: reviewer_ladder(Arc::new(ScriptedAdapter::failing(
            Provider::Gemini,
            "reviewer down",
        ))),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(dir.path().to_path_buf());

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
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![
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
        reviewer: Vec::new(),
        chairman: chairman_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Claude,
            "combined answer",
        ))),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(dir.path().to_path_buf());

    let out = server
        .council_run_inner(council_params("which option?", dir.path()))
        .await;

    assert!(out.ok, "error={:?}", out.error);
    assert!(out.synthesis.is_some(), "expected chairman synthesis");
    assert!(!out.answers.is_empty(), "expected member answers");
}

#[tokio::test]
async fn council_run_empty_question_returns_error() {
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    });
    let out = server
        .council_run_inner(council_params("   ", std::path::Path::new("/tmp")))
        .await;
    assert!(!out.ok);
    assert!(out.error.unwrap_or_default().contains("empty question"));
}

// ─── cwd confinement ──────────────────────────────────────────────────────────
//
// The MCP caller is an LLM conductor reading untrusted repo content, so a
// prompt injection can supply any `cwd`. Every cwd-taking tool must confine it
// to the launch root (mirrors the WS server's cwd_within_root check) — else
// run_worker points a write-enabled worker at ~/.ssh and run_verify executes
// `make test` in a model-chosen directory.

fn single_worker_server(log: &CallLog, launch_root: &std::path::Path) -> McpServer {
    let rec = Arc::new(RecordingAdapter::new(
        ScriptedAdapter::ok_with_text(Provider::Codex, "should not have run"),
        log.clone(),
    ));
    McpServer::from_parts(McpServerDeps {
        workers: vec![worker("codex-gpt", rec)],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(launch_root.to_path_buf())
}

#[tokio::test]
async fn run_worker_cwd_outside_launch_root_is_rejected() {
    let root = temp_repo();
    let outside = temp_repo(); // a perfectly valid repo — but not under the root
    let log: CallLog = Arc::new(Mutex::new(Vec::new()));
    let server = single_worker_server(&log, root.path());

    let out = server
        .run_worker_inner(params("codex-gpt", outside.path()))
        .await;

    assert!(!out.ok, "cwd outside the launch root must be refused");
    let err = out.error.unwrap_or_default();
    assert!(err.contains("outside"), "got: {err}");
    assert!(
        out.verify.is_none(),
        "verify must not run in a rejected cwd"
    );
    assert!(
        log.lock().unwrap().is_empty(),
        "the worker must never launch in an unconfined cwd"
    );
}

#[tokio::test]
async fn run_worker_nonexistent_cwd_is_rejected() {
    let root = temp_repo();
    let missing = root.path().join("does_not_exist");
    let log: CallLog = Arc::new(Mutex::new(Vec::new()));
    let server = single_worker_server(&log, root.path());

    let out = server.run_worker_inner(params("codex-gpt", &missing)).await;

    assert!(
        !out.ok,
        "a cwd that cannot be canonicalized must be refused"
    );
    let err = out.error.unwrap_or_default();
    assert!(err.contains("outside"), "got: {err}");
    assert!(log.lock().unwrap().is_empty(), "worker must not launch");
}

#[tokio::test]
async fn run_worker_path_escape_cwd_is_rejected() {
    // `root/sub/../..` textually starts inside the root but canonicalizes to the
    // root's parent — the traversal must be resolved before the containment check.
    let root = temp_repo();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    let escape = root.path().join("sub").join("..").join("..");
    let log: CallLog = Arc::new(Mutex::new(Vec::new()));
    let server = single_worker_server(&log, root.path());

    let out = server.run_worker_inner(params("codex-gpt", &escape)).await;

    assert!(!out.ok, "a `..` escape must be refused");
    let err = out.error.unwrap_or_default();
    assert!(err.contains("outside"), "got: {err}");
    assert!(log.lock().unwrap().is_empty(), "worker must not launch");
}

#[tokio::test]
async fn run_worker_cwd_subdir_of_launch_root_is_allowed() {
    // Positive control: confinement must not over-restrict legitimate subdirs.
    let root = temp_repo();
    let sub = root.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![worker(
            "codex-gpt",
            Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "did it")),
        )],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(root.path().to_path_buf());

    let out = server.run_worker_inner(params("codex-gpt", &sub)).await;

    assert!(out.ok, "subdir of the root is fine; error={:?}", out.error);
}

#[tokio::test]
async fn review_diff_cwd_outside_launch_root_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![],
        reviewer: reviewer_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Gemini,
            r#"{"findings":[]}"#,
        ))),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(root.path().to_path_buf());

    let out = server
        .review_diff_inner(review_params("some diff", outside.path()))
        .await;

    assert!(!out.ok, "cwd outside the launch root must be refused");
    let err = out.error.unwrap_or_default();
    assert!(err.contains("outside"), "got: {err}");
    assert!(out.raw_review.is_none(), "the reviewer must never run");
}

#[tokio::test]
async fn council_run_cwd_outside_launch_root_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![worker(
            "codex-gpt",
            Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "answer")),
        )],
        reviewer: Vec::new(),
        chairman: chairman_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Claude,
            "synthesis",
        ))),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(root.path().to_path_buf());

    let out = server
        .council_run_inner(council_params("which option?", outside.path()))
        .await;

    assert!(!out.ok, "cwd outside the launch root must be refused");
    let err = out.error.unwrap_or_default();
    assert!(err.contains("outside"), "got: {err}");
    assert!(out.synthesis.is_none(), "the council must never run");
    assert!(out.answers.is_empty());
}

#[tokio::test]
async fn review_diff_default_cwd_is_launch_root_and_allowed() {
    // Omitted cwd falls back to the launch root itself, which trivially passes.
    let root = tempfile::tempdir().unwrap();
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![],
        reviewer: reviewer_ladder(Arc::new(ScriptedAdapter::ok_with_text(
            Provider::Gemini,
            r#"{"findings":[]}"#,
        ))),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(root.path().to_path_buf());

    let out = server
        .review_diff_inner(ReviewDiffParams {
            diff: "some diff".into(),
            cwd: None,
            timeout_secs: Some(30),
            author_model: None,
            task: None,
            cross_examine: None,
        })
        .await;

    assert!(out.ok, "default cwd must be usable; error={:?}", out.error);
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

    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: base.path().to_path_buf(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    });

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

    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: base.path().to_path_buf(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    });

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

// ─── failure memory ───────────────────────────────────────────────────────────

/// The server lives for hours. What made a model unusable an hour ago — a
/// plan change, a sign-out, a throttle — may be fixed by now, and a model the
/// server once saw fail must not stay skipped until the server is restarted.
#[tokio::test]
async fn a_model_that_failed_is_tried_again_on_the_next_call() {
    let repo = temp_repo();
    // codex-cli's real refusal for a model the plan does not include: the
    // failover marks such a model dead for the rest of the run.
    let flaky = common::SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter::failing(
                Provider::Codex,
                "The 'gpt' model is not supported when using Codex with a ChatGPT account.",
            ),
            ScriptedAdapter::ok_with_text(Provider::Codex, "works now"),
        ],
    );
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![worker("codex-gpt", Arc::new(flaky))],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(repo.path().to_path_buf());

    let first = server
        .run_worker_inner(params("codex-gpt", repo.path()))
        .await;
    assert!(!first.ok, "the first call fails");

    let second = server
        .run_worker_inner(params("codex-gpt", repo.path()))
        .await;
    assert!(second.ok, "error={:?}", second.error);
    assert_eq!(second.worker_report.as_deref(), Some("works now"));
}

// ─── cross-family review ──────────────────────────────────────────────────────

const CLEAN_VERDICT: &str = "```json\n{\"findings\":[]}\n```";

fn clean_reviewer(provider: Provider) -> Arc<dyn consilium::adapters::Adapter> {
    Arc::new(ScriptedAdapter::ok_with_text(provider, CLEAN_VERDICT))
}

fn review_server(
    reviewer: Vec<Rung>,
    workers: Vec<CouncilMember>,
    root: &std::path::Path,
) -> McpServer {
    McpServer::from_parts(McpServerDeps {
        workers,
        reviewer,
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(root.to_path_buf())
}

fn review_by(author_model: Option<&str>, cwd: &std::path::Path) -> ReviewDiffParams {
    ReviewDiffParams {
        author_model: author_model.map(String::from),
        ..review_params("diff --git a/x b/x\n+x", cwd)
    }
}

/// `/consilium:review` promises "a reviewer of a different model family". The
/// tool ran the reviewer ladder exactly as configured, whoever wrote the diff.
#[tokio::test]
async fn review_diff_puts_a_different_family_than_the_author_first() {
    let repo = temp_repo();
    let server = review_server(
        vec![
            rung(
                Provider::Codex,
                "gpt-6-luna",
                clean_reviewer(Provider::Codex),
            ),
            rung(
                Provider::Gemini,
                "gemini-4-argon-high",
                clean_reviewer(Provider::Gemini),
            ),
        ],
        Vec::new(),
        repo.path(),
    );

    let out = server
        .review_diff_inner(review_by(Some("codex/gpt-6.1-sol"), repo.path()))
        .await;

    assert!(out.ok, "error={:?}", out.error);
    assert_eq!(
        out.model_used.as_deref(),
        Some("gemini/gemini-4-argon-high")
    );
    assert_eq!(out.cross_family.as_deref(), Some("applied"));
}

/// Claude served through Antigravity is still Claude.
#[tokio::test]
async fn review_diff_sees_the_authors_family_through_a_gateway() {
    let repo = temp_repo();
    let server = review_server(
        vec![
            rung(
                Provider::Claude,
                "claude-opus-5-5",
                clean_reviewer(Provider::Claude),
            ),
            rung(
                Provider::Codex,
                "gpt-6-luna",
                clean_reviewer(Provider::Codex),
            ),
        ],
        Vec::new(),
        repo.path(),
    );

    let out = server
        .review_diff_inner(review_by(Some("gemini/claude-opus-5-5-high"), repo.path()))
        .await;

    assert_eq!(out.model_used.as_deref(), Some("codex/gpt-6-luna"));
}

/// The attached conductor is a Claude session: a diff nobody is named for is
/// its own work, and must not be reviewed by Claude first either.
#[tokio::test]
async fn review_diff_takes_an_unattributed_diff_for_the_conductors_own() {
    let repo = temp_repo();
    let server = review_server(
        vec![
            rung(
                Provider::Claude,
                "claude-opus-5-5",
                clean_reviewer(Provider::Claude),
            ),
            rung(
                Provider::Codex,
                "gpt-6-luna",
                clean_reviewer(Provider::Codex),
            ),
        ],
        Vec::new(),
        repo.path(),
    );

    let out = server.review_diff_inner(review_by(None, repo.path())).await;

    assert_eq!(out.model_used.as_deref(), Some("codex/gpt-6-luna"));
    assert_eq!(out.cross_family.as_deref(), Some("applied"));
}

/// The conductor has a worker's label at hand as readily as its model. A
/// label that names no family ("codex-default") used to be read as "no
/// author", and Codex reviewed a Codex diff under `cross_family: applied`.
#[tokio::test]
async fn review_diff_reads_a_worker_label_as_the_author() {
    let repo = temp_repo();
    let server = review_server(
        vec![
            rung(
                Provider::Codex,
                "gpt-6-luna",
                clean_reviewer(Provider::Codex),
            ),
            rung(
                Provider::Gemini,
                "gemini-4-argon-high",
                clean_reviewer(Provider::Gemini),
            ),
        ],
        vec![CouncilMember {
            label: "codex-default".into(),
            ladder: vec![rung(
                Provider::Codex,
                "default",
                clean_reviewer(Provider::Codex),
            )],
        }],
        repo.path(),
    );

    let out = server
        .review_diff_inner(review_by(Some("codex-default"), repo.path()))
        .await;

    assert_eq!(
        out.model_used.as_deref(),
        Some("gemini/gemini-4-argon-high")
    );
    assert_eq!(out.cross_family.as_deref(), Some("applied"));
}

/// An author the server cannot place must not be passed off as checked.
#[tokio::test]
async fn review_diff_says_when_it_cannot_tell_who_the_author_is() {
    let repo = temp_repo();
    let server = review_server(
        vec![rung(
            Provider::Codex,
            "gpt-6-luna",
            clean_reviewer(Provider::Codex),
        )],
        Vec::new(),
        repo.path(),
    );

    let out = server
        .review_diff_inner(review_by(Some("mystery-model"), repo.path()))
        .await;

    assert!(out.ok, "the review still runs: {:?}", out.error);
    assert_eq!(out.cross_family.as_deref(), Some("unknown_author"));
}

/// With no other family in the lineup the review still runs — and says so.
#[tokio::test]
async fn review_diff_says_when_only_the_authors_family_could_review() {
    let repo = temp_repo();
    let server = review_server(
        vec![rung(
            Provider::Claude,
            "claude-opus-5-5",
            clean_reviewer(Provider::Claude),
        )],
        Vec::new(),
        repo.path(),
    );

    let out = server.review_diff_inner(review_by(None, repo.path())).await;

    assert!(out.ok, "error={:?}", out.error);
    assert_eq!(out.model_used.as_deref(), Some("claude/claude-opus-5-5"));
    assert_eq!(out.cross_family.as_deref(), Some("degraded_same_family"));
}

/// A single same-family reviewer borrows a worker of another family, as the
/// `conduct` review gate does.
#[tokio::test]
async fn review_diff_borrows_a_worker_of_another_family() {
    let repo = temp_repo();
    let server = review_server(
        vec![rung(
            Provider::Claude,
            "claude-opus-5-5",
            clean_reviewer(Provider::Claude),
        )],
        vec![CouncilMember {
            label: "gemini-gemini-4-argon-high".into(),
            ladder: vec![rung(
                Provider::Gemini,
                "gemini-4-argon-high",
                clean_reviewer(Provider::Gemini),
            )],
        }],
        repo.path(),
    );

    let out = server.review_diff_inner(review_by(None, repo.path())).await;

    assert_eq!(
        out.model_used.as_deref(),
        Some("gemini/gemini-4-argon-high")
    );
    assert_eq!(out.cross_family.as_deref(), Some("applied"));
}

// ─── a worker's own changes ───────────────────────────────────────────────────

/// A repository whose tree is already dirty when the worker starts: one
/// tracked file edited and one new file, neither committed — the conductor's
/// own work in progress.
fn repo_with_conductor_edits() -> tempfile::TempDir {
    let repo = temp_repo();
    std::fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    git(repo.path(), &["add", "base.txt"]);
    git(repo.path(), &["commit", "-qm", "base"]);
    std::fs::write(repo.path().join("base.txt"), "conductor edit\n").unwrap();
    std::fs::write(repo.path().join("conductor-notes.txt"), "mine\n").unwrap();
    repo
}

fn scripted_worker_server(pre_script: &str, root: &std::path::Path) -> McpServer {
    let adapter = ScriptedAdapter {
        pre_script: pre_script.into(),
        ..ScriptedAdapter::ok_with_text(Provider::Codex, "done")
    };
    McpServer::from_parts(McpServerDeps {
        workers: vec![worker("codex-gpt", Arc::new(adapter))],
        reviewer: Vec::new(),
        chairman: Vec::new(),
        transcript_base: scratch_transcripts(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(root.to_path_buf())
}

/// `changes` used to be everything uncommitted in the tree. From the second
/// call on that mixed the conductor's own edits and earlier workers' edits
/// into what this worker was said to have done.
#[tokio::test]
async fn run_worker_reports_only_what_the_worker_changed() {
    let repo = repo_with_conductor_edits();
    let server = scripted_worker_server(
        "echo 'fn worker() {}' > worker.rs; echo 'worker line' >> base.txt",
        repo.path(),
    );

    let out = server
        .run_worker_inner(params("codex-gpt", repo.path()))
        .await;

    let changes = out.changes.expect("a git repository yields changes");
    assert!(changes.contains("worker.rs"), "{changes}");
    assert!(
        changes.contains("+fn worker() {}"),
        "a new file's content: {changes}"
    );
    assert!(changes.contains("+worker line"), "{changes}");
    // what was already there before the worker ran is not the worker's
    assert!(!changes.contains("conductor-notes.txt"), "{changes}");
    assert!(!changes.contains("+conductor edit"), "{changes}");
}

#[tokio::test]
async fn run_worker_that_edits_nothing_reports_no_changes_in_a_dirty_tree() {
    let repo = repo_with_conductor_edits();
    let server = scripted_worker_server("true", repo.path());

    let out = server
        .run_worker_inner(params("codex-gpt", repo.path()))
        .await;

    assert_eq!(out.changes.as_deref(), Some("(no changes)"));
}

/// Taking the snapshots must leave the repository as it was: nothing staged,
/// the conductor's files untouched.
#[tokio::test]
async fn run_worker_leaves_the_index_alone() {
    let repo = repo_with_conductor_edits();
    let status = |repo: &tempfile::TempDir| {
        let out = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    };
    let before = status(&repo);
    let server = scripted_worker_server("true", repo.path());

    server
        .run_worker_inner(params("codex-gpt", repo.path()))
        .await;

    assert_eq!(status(&repo), before);
    assert_eq!(before, " M base.txt\n?? conductor-notes.txt\n");
}

/// Two write workers in one directory edit one tree: run together, each would
/// be shown the other's half-made changes and verify a tree in motion.
#[tokio::test]
async fn write_workers_in_one_repository_take_turns() {
    let repo = temp_repo();
    let log_dir = tempfile::tempdir().unwrap();
    let log = log_dir.path().join("turns.log");
    let script = format!(
        "echo start >> '{log}'; sleep 1; echo end >> '{log}'",
        log = log.display()
    );
    let server = scripted_worker_server(&script, repo.path());

    let (a, b) = tokio::join!(
        server.run_worker_inner(params("codex-gpt", repo.path())),
        server.run_worker_inner(params("codex-gpt", repo.path())),
    );

    assert!(a.ok && b.ok, "a={:?} b={:?}", a.error, b.error);
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "start\nend\nstart\nend\n"
    );
}

// ─── cross-examination of a review's findings ───────────────────────────────

const CRITICAL_AND_IMPORTANT: &str = r#"{"findings":[{"severity":"critical","file":"avg.rs","description":"divides by zero"},{"severity":"important","file":"avg.rs","description":"misleading name"}]}"#;

const CRITICAL_REFUTED: &str = r#"{"rulings":[{"finding":1,"ruling":"refuted","reason":"n is checked two lines above"},{"finding":2,"ruling":"confirmed","reason":"it returns the median"}]}"#;

const CRITICAL_CONFIRMED: &str = r#"{"rulings":[{"finding":1,"ruling":"confirmed","reason":"an empty slice gives n = 0"},{"finding":2,"ruling":"unclear","reason":"a matter of taste"}]}"#;

/// Codex reviews and reports a critical and an important finding; Gemini is
/// there to rule on them and answers `examiner_reply`. Returns the server,
/// the prompts Gemini was given, and where the server saves its runs.
fn examined_review_server(
    examiner_reply: &str,
    root: &std::path::Path,
) -> (McpServer, CallLog, std::path::PathBuf) {
    let examiner_calls: CallLog = Arc::default();
    let examiner = RecordingAdapter::new(
        ScriptedAdapter::ok_with_text(Provider::Gemini, examiner_reply),
        examiner_calls.clone(),
    );
    let transcript_base = scratch_transcripts();
    let server = McpServer::from_parts(McpServerDeps {
        workers: vec![CouncilMember {
            label: "gemini".into(),
            ladder: vec![rung(
                Provider::Gemini,
                "gemini-4-argon-high",
                Arc::new(examiner),
            )],
        }],
        reviewer: vec![rung(
            Provider::Codex,
            "gpt-6-luna",
            Arc::new(ScriptedAdapter::ok_with_text(
                Provider::Codex,
                CRITICAL_AND_IMPORTANT,
            )),
        )],
        chairman: Vec::new(),
        transcript_base: transcript_base.clone(),
        verify: None,
        quota: QuotaStore::open_in_memory().unwrap(),
    })
    .with_launch_root(root.to_path_buf());
    (server, examiner_calls, transcript_base)
}

fn rulings_of(out: &consilium::mcp::ReviewDiffOutput) -> Vec<Option<&str>> {
    out.findings.iter().map(|f| f.ruling.as_deref()).collect()
}

/// A reviewer told to hunt for problems also reports some that are not
/// there. Its findings no longer reach the conductor unchecked: a model with
/// no stake in the review rules on each one worth acting on.
#[tokio::test]
async fn review_diff_has_a_second_model_rule_on_the_findings() {
    let repo = temp_repo();
    let (server, examiner_calls, _) = examined_review_server(CRITICAL_REFUTED, repo.path());

    let out = server
        .review_diff_inner(review_by(Some("claude/claude-fable-5-2"), repo.path()))
        .await;

    assert!(out.ok, "error={:?}", out.error);
    assert_eq!(rulings_of(&out), [Some("refuted"), Some("confirmed")]);
    assert_eq!(
        out.findings[0].ruling_reason.as_deref(),
        Some("n is checked two lines above")
    );
    assert_eq!(out.cross_examination.as_deref(), Some("examined"));
    assert_eq!(
        out.examined_by.as_deref(),
        Some("gemini/gemini-4-argon-high")
    );
    assert_eq!(out.examiner_independence.as_deref(), Some("independent"));
    assert_eq!(examiner_calls.lock().unwrap().len(), 1);
}

/// A false alarm marked critical used to cost a rework round. Refuted by a
/// model with no stake, it is still reported — but it no longer blocks.
#[tokio::test]
async fn a_critical_finding_blocks_only_while_it_stands() {
    let repo = temp_repo();

    let (refuting, _, _) = examined_review_server(CRITICAL_REFUTED, repo.path());
    let refuted = refuting
        .review_diff_inner(review_by(Some("claude/claude-fable-5-2"), repo.path()))
        .await;
    assert!(refuted.has_critical, "the reviewer did report a critical");
    assert!(!refuted.blocking);

    let (confirming, _, _) = examined_review_server(CRITICAL_CONFIRMED, repo.path());
    let confirmed = confirming
        .review_diff_inner(review_by(Some("claude/claude-fable-5-2"), repo.path()))
        .await;
    assert!(confirmed.has_critical);
    assert!(confirmed.blocking);
}

#[tokio::test]
async fn review_diff_skips_the_cross_examination_when_told_to() {
    let repo = temp_repo();
    let (server, examiner_calls, _) = examined_review_server(CRITICAL_REFUTED, repo.path());
    let params = ReviewDiffParams {
        cross_examine: Some(false),
        ..review_by(None, repo.path())
    };

    let out = server.review_diff_inner(params).await;

    assert!(out.ok, "error={:?}", out.error);
    assert_eq!(rulings_of(&out), [None, None]);
    assert_eq!(out.cross_examination.as_deref(), Some("off"));
    assert!(out.blocking, "an unexamined critical blocks");
    assert!(examiner_calls.lock().unwrap().is_empty());
}

/// An examination that cannot be had costs nothing but itself: the review is
/// returned as the reviewer wrote it, and says why no one ruled on it.
#[tokio::test]
async fn a_failed_cross_examination_leaves_the_review_standing() {
    let repo = temp_repo();
    let (server, _, _) = examined_review_server("They all look plausible.", repo.path());

    let out = server.review_diff_inner(review_by(None, repo.path())).await;

    assert!(out.ok, "error={:?}", out.error);
    assert_eq!(out.findings.len(), 2);
    assert_eq!(rulings_of(&out), [None, None]);
    let status = out.cross_examination.as_deref().unwrap_or_default();
    assert!(status.starts_with("unavailable: "), "{status}");
    assert!(out.blocking);
}

#[tokio::test]
async fn a_clean_review_reports_nothing_to_examine() {
    let repo = temp_repo();
    let server = review_server(
        vec![rung(
            Provider::Codex,
            "gpt-6-luna",
            clean_reviewer(Provider::Codex),
        )],
        Vec::new(),
        repo.path(),
    );

    let out = server.review_diff_inner(review_by(None, repo.path())).await;

    assert_eq!(out.cross_examination.as_deref(), Some("nothing_to_examine"));
    assert!(!out.blocking);
}

/// What `standings` counts from: the saved review names who wrote the diff,
/// who reviewed it, who ruled, and each finding with its ruling.
#[tokio::test]
async fn the_saved_review_names_the_parties_and_each_ruling() {
    let repo = temp_repo();
    let (server, _, transcript_base) = examined_review_server(CRITICAL_REFUTED, repo.path());

    let out = server
        .review_diff_inner(review_by(Some("claude/claude-fable-5-2"), repo.path()))
        .await;

    let id = out.transcript_id.expect("the review is saved");
    let saved = consilium::orchestrator::transcript::TranscriptStore::new(transcript_base)
        .load_by_id(&id)
        .expect("the saved review loads");
    assert_eq!(saved["author_model"], "claude/claude-fable-5-2");
    assert_eq!(saved["model_used"], "codex/gpt-6-luna");
    assert_eq!(saved["examined_by"], "gemini/gemini-4-argon-high");
    assert_eq!(saved["examiner_independence"], "independent");
    assert_eq!(saved["cross_family"], "applied");
    assert_eq!(saved["findings"][0]["severity"], "critical");
    assert_eq!(saved["findings"][0]["ruling"], "refuted");
    assert_eq!(saved["findings"][1]["ruling"], "confirmed");
}

// ─── standings ──────────────────────────────────────────────────────────────

/// The scoreboard is counted from what the tools themselves saved: after one
/// cross-examined review, the reviewer has a confirmed finding and a refuted
/// one to its name, and the examiner two rulings.
#[tokio::test]
async fn standings_are_counted_from_the_runs_the_server_saved() {
    let repo = temp_repo();
    let (server, _, _) = examined_review_server(CRITICAL_REFUTED, repo.path());
    server
        .review_diff_inner(review_by(Some("claude/claude-fable-5-2"), repo.path()))
        .await;

    let standings = server.standings_inner(consilium::mcp::StandingsParams::default());

    assert_eq!(standings.runs, 1);
    let reviewer = &standings.reviewers[0];
    assert_eq!(reviewer.model, "codex/gpt-6-luna");
    assert_eq!(
        (reviewer.reviews, reviewer.confirmed, reviewer.refuted),
        (1, 1, 1)
    );
    let examiner = &standings.examiners[0];
    assert_eq!(examiner.model, "gemini/gemini-4-argon-high");
    assert_eq!(examiner.examinations, 1);
}

#[tokio::test]
async fn standings_can_be_limited_to_recent_runs() {
    let repo = temp_repo();
    let (server, _, transcript_base) = examined_review_server(CRITICAL_REFUTED, repo.path());
    let runs = transcript_base.join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    // a review saved long ago: the file name carries the time it was saved
    std::fs::write(
        runs.join("1000000000000000000-0000-review.json"),
        r#"{"kind":"review","model_used":"codex/gpt-5.5"}"#,
    )
    .unwrap();

    let recent = server.standings_inner(consilium::mcp::StandingsParams { days: Some(30) });
    let all = server.standings_inner(consilium::mcp::StandingsParams { days: None });

    assert_eq!(recent.runs, 0);
    assert_eq!(all.runs, 1);
    assert_eq!(all.reviewers[0].model, "codex/gpt-5.5");
}

/// A diff can be flawless and still not be what was asked for. A reviewer
/// that is told the task can say so.
#[tokio::test]
async fn review_diff_tells_the_reviewer_what_the_diff_was_for() {
    let repo = temp_repo();
    let reviewer_prompts: CallLog = Arc::default();
    let reviewer = RecordingAdapter::new(
        ScriptedAdapter::ok_with_text(Provider::Codex, CLEAN_VERDICT),
        reviewer_prompts.clone(),
    );
    let server = review_server(
        vec![rung(Provider::Codex, "gpt-6-luna", Arc::new(reviewer))],
        Vec::new(),
        repo.path(),
    );
    let params = ReviewDiffParams {
        task: Some("add a retry budget to fetch()".into()),
        ..review_by(None, repo.path())
    };

    let out = server.review_diff_inner(params).await;

    assert!(out.ok, "error={:?}", out.error);
    let prompts = reviewer_prompts.lock().unwrap();
    assert!(
        prompts[0]
            .0
            .contains("<task>\nadd a retry budget to fetch()\n</task>"),
        "{}",
        prompts[0].0
    );
}

/// The review said who ruled but not that the model first in line had been
/// passed over, or why — so an examiner of the author's own family could
/// stand in for an independent one without a word.
#[tokio::test]
async fn review_diff_names_every_model_it_passed_over() {
    let repo = temp_repo();
    let member = |label: &str, rung: Rung| CouncilMember {
        label: label.into(),
        ladder: vec![rung],
    };
    let server = review_server(
        vec![rung(
            Provider::Codex,
            "gpt-6-luna",
            Arc::new(ScriptedAdapter::ok_with_text(
                Provider::Codex,
                CRITICAL_AND_IMPORTANT,
            )),
        )],
        vec![
            member(
                "gemini",
                rung(
                    Provider::Gemini,
                    "gemini-retired",
                    Arc::new(ScriptedAdapter::failing(
                        Provider::Gemini,
                        "invalid model selection",
                    )),
                ),
            ),
            member(
                "claude",
                rung(
                    Provider::Claude,
                    "claude-fable-5-2",
                    Arc::new(ScriptedAdapter::ok_with_text(
                        Provider::Claude,
                        CRITICAL_CONFIRMED,
                    )),
                ),
            ),
        ],
        repo.path(),
    );

    let out = server
        .review_diff_inner(review_by(Some("claude/claude-fable-5-2"), repo.path()))
        .await;

    assert_eq!(out.examined_by.as_deref(), Some("claude/claude-fable-5-2"));
    assert_eq!(out.examiner_independence.as_deref(), Some("authors_family"));
    assert_eq!(out.fallbacks.len(), 1, "{:?}", out.fallbacks);
    assert!(
        out.fallbacks[0].starts_with("gemini/gemini-retired → claude/claude-fable-5-2"),
        "{:?}",
        out.fallbacks
    );
}

/// An author the server cannot place is no author to be independent of: the
/// examiner may be on the author's side for all anyone knows, so its
/// refutation is reported but lifts nothing.
#[tokio::test]
async fn a_refutation_lifts_nothing_when_the_author_is_unknown() {
    let repo = temp_repo();
    let (server, _, _) = examined_review_server(CRITICAL_REFUTED, repo.path());

    let out = server
        .review_diff_inner(review_by(Some("mystery-model"), repo.path()))
        .await;

    assert_eq!(out.cross_family.as_deref(), Some("unknown_author"));
    assert_eq!(rulings_of(&out), [Some("refuted"), Some("confirmed")]);
    assert!(out.blocking, "the critical finding stands");
    assert_eq!(out.examiner_independence.as_deref(), Some("unknown_author"));
}

/// `blocking` alone must be safe to act on: a review that could not be read
/// is not a clean one.
#[tokio::test]
async fn an_unreadable_review_blocks() {
    let repo = temp_repo();
    let server = review_server(
        vec![rung(
            Provider::Codex,
            "gpt-6-luna",
            Arc::new(ScriptedAdapter::ok_with_text(
                Provider::Codex,
                "Looks risky to me.",
            )),
        )],
        Vec::new(),
        repo.path(),
    );

    let out = server.review_diff_inner(review_by(None, repo.path())).await;

    assert!(out.ok && !out.parse_ok);
    assert!(out.blocking);
}

/// The examiner rules on a finding about the task only if it knows the task.
#[tokio::test]
async fn review_diff_tells_the_examiner_what_the_diff_was_for() {
    let repo = temp_repo();
    let (server, examiner_calls, _) = examined_review_server(CRITICAL_REFUTED, repo.path());
    let params = ReviewDiffParams {
        task: Some("add a retry budget to fetch()".into()),
        ..review_by(None, repo.path())
    };

    server.review_diff_inner(params).await;

    let calls = examiner_calls.lock().unwrap();
    assert!(
        calls[0]
            .0
            .contains("<task>\nadd a retry budget to fetch()\n</task>"),
        "{}",
        calls[0].0
    );
}

/// "Taken to be your own" was a fair guess for choosing who reviews first.
/// It is not good enough for waving a critical finding through: a worker's
/// diff sent without its author would have its own family pass for an
/// independent examiner. Unless the author is named, nothing is lifted.
#[tokio::test]
async fn a_refutation_lifts_nothing_unless_the_author_is_named() {
    let repo = temp_repo();
    let (server, _, _) = examined_review_server(CRITICAL_REFUTED, repo.path());

    let out = server.review_diff_inner(review_by(None, repo.path())).await;

    assert_eq!(rulings_of(&out), [Some("refuted"), Some("confirmed")]);
    assert!(out.blocking, "no author was named, so the finding stands");
    assert_eq!(out.examiner_independence.as_deref(), Some("unknown_author"));
}
