mod common;

use common::{ScriptedAdapter, SequencedAdapter};
use consilium::config::ModelCandidate;
use consilium::event::Provider;
use consilium::orchestrator::conduct::{ConductDeps, RoleHandle};
use consilium::orchestrator::council::CouncilMember;
use consilium::orchestrator::resilience::Rung;
use consilium::quota::QuotaStore;
use consilium::server::{router, ServerState};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

fn git(dir: &std::path::Path, args: &[&str]) {
    assert!(std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t.com")
        .output()
        .unwrap()
        .status
        .success());
}

fn temp_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["commit", "--allow-empty", "-m", "init", "-q"]);
    dir
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

// End-to-end: a browser opens the WS, sends a conduct frame, and receives the
// run's events live followed by a `run_complete` terminal frame — proving the
// task-local sink fans through the axum WebSocket. Scripted adapters → zero quota.
#[tokio::test]
async fn ws_streams_conduct_events_then_terminal_frame() {
    let repo = temp_repo();

    // Fresh scripted ConductDeps per run (one connection ⇒ called once).
    let deps_fn: Arc<dyn Fn() -> ConductDeps + Send + Sync> = Arc::new(|| {
        let plan =
            r#"{"subtasks":[{"id":1,"title":"x","prompt":"write out.txt","depends_note":""}]}"#;
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
            pre_script: "echo hi > out.txt".into(),
            ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it")
        });
        ConductDeps {
            conductor: RoleHandle {
                ladder: vec![rung(Provider::Claude, "m", conductor)],
            },
            workers: vec![CouncilMember {
                label: "codex-gpt".into(),
                ladder: vec![rung(Provider::Codex, "gpt", worker)],
            }],
            supervisor: None,
            reviewer: None,
            arbiter: None,
            verify: None,
            memory: Default::default(),
        }
    });

    let state = ServerState::from_parts(
        deps_fn,
        QuotaStore::open_in_memory().unwrap(),
        Duration::from_secs(30),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/session"))
        .await
        .unwrap();
    let frame = serde_json::json!({
        "kind": "conduct", "task": "t", "context": "",
        "cwd": repo.path().to_string_lossy(),
    })
    .to_string();
    ws.send(Message::Text(frame.into())).await.unwrap();

    let mut texts: Vec<String> = Vec::new();
    while let Some(Ok(msg)) = ws.next().await {
        match msg {
            Message::Text(t) => texts.push(t.to_string()),
            Message::Close(_) => break,
            _ => {}
        }
    }

    // A terminal run_complete frame with the accepted subtask.
    let term = texts
        .iter()
        .find(|t| t.contains("\"type\":\"run_complete\""))
        .unwrap_or_else(|| panic!("no run_complete frame; got {texts:?}"));
    assert!(term.contains("\"completed\":[1]"), "terminal frame: {term}");
    // Live event frames arrived before the terminal frame (not just the summary).
    assert!(
        texts.iter().any(|t| t.contains("\"type\":\"")
            && !t.contains("run_complete")
            && !t.contains("run_error")),
        "expected live AgentEvent frames; got {texts:?}"
    );
    assert!(repo.path().join("out.txt").exists());
}
