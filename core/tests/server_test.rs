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
    // Hermetic identity: bare CI runners have no global user.name/email.
    git(dir.path(), &["config", "user.name", "consilium-test"]);
    git(
        dir.path(),
        &["config", "user.email", "test@consilium.local"],
    );
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
            cross_family_review: false,
            max_replans: 0,
            budget: None,
        }
    });

    let state = ServerState::from_parts(
        deps_fn,
        QuotaStore::open_in_memory().unwrap(),
        Duration::from_secs(30),
    )
    .with_launch_root(repo.path().to_path_buf());
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

// Collect text frames from a connected socket until it closes.
async fn drain(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Vec<String> {
    let mut texts = Vec::new();
    while let Some(Ok(msg)) = ws.next().await {
        match msg {
            Message::Text(t) => texts.push(t.to_string()),
            Message::Close(_) => break,
            _ => {}
        }
    }
    texts
}

async fn spawn_server(state: ServerState) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });
    addr
}

// A malformed first frame gets a structured `error` frame, then the socket
// closes — and the run is never started (deps_fn must not be called).
#[tokio::test]
async fn ws_bad_first_frame_returns_error_then_closes() {
    let deps_fn: Arc<dyn Fn() -> ConductDeps + Send + Sync> =
        Arc::new(|| panic!("deps_fn must not be called when the first frame is invalid"));
    let state = ServerState::from_parts(
        deps_fn,
        QuotaStore::open_in_memory().unwrap(),
        Duration::from_secs(30),
    );
    let addr = spawn_server(state).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/session"))
        .await
        .unwrap();
    ws.send(Message::Text("this is not json".into()))
        .await
        .unwrap();

    let texts = drain(&mut ws).await;
    assert!(
        texts.iter().any(|t| t.contains("\"type\":\"error\"")),
        "expected an error frame; got {texts:?}"
    );
}

// When the run fails (the conductor's only rung errors), the terminal frame is
// `run_error` and the socket still closes cleanly.
#[tokio::test]
async fn ws_run_error_terminal_frame_on_failed_run() {
    let repo = temp_repo();
    let deps_fn: Arc<dyn Fn() -> ConductDeps + Send + Sync> = Arc::new(|| ConductDeps {
        conductor: RoleHandle {
            ladder: vec![rung(
                Provider::Claude,
                "m",
                Arc::new(ScriptedAdapter::failing(Provider::Claude, "boom")),
            )],
        },
        workers: vec![CouncilMember {
            label: "codex-gpt".into(),
            ladder: vec![rung(
                Provider::Codex,
                "gpt",
                Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "unused")),
            )],
        }],
        supervisor: None,
        reviewer: None,
        arbiter: None,
        verify: None,
        memory: Default::default(),
        cross_family_review: false,
        max_replans: 0,
        budget: None,
    });
    let state = ServerState::from_parts(
        deps_fn,
        QuotaStore::open_in_memory().unwrap(),
        Duration::from_secs(30),
    )
    .with_launch_root(repo.path().to_path_buf());
    let addr = spawn_server(state).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws/session"))
        .await
        .unwrap();
    let frame = serde_json::json!({
        "kind": "conduct", "task": "t", "cwd": repo.path().to_string_lossy(),
    })
    .to_string();
    ws.send(Message::Text(frame.into())).await.unwrap();

    let texts = drain(&mut ws).await;
    assert!(
        texts.iter().any(|t| t.contains("\"type\":\"run_error\"")),
        "expected a run_error terminal frame; got {texts:?}"
    );
}
