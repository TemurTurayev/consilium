//! Integration tests for the SessionManager (real process spawning).

mod common;
use common::ScriptedAdapter;

use consilium::adapters::{Adapter, RunRequest};
use consilium::event::{AgentEvent, Provider};
use consilium::sessions;
use std::sync::Arc;

/// Fake adapter whose process exits non-zero without output.
struct CrashingAdapter;

impl Adapter for CrashingAdapter {
    fn provider(&self) -> Provider {
        Provider::Claude
    }
    fn cli_binary(&self) -> &'static str {
        "sh"
    }
    fn build_command(&self, _req: &RunRequest) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg("exit 3");
        cmd
    }
}

fn req() -> RunRequest {
    RunRequest {
        prompt: "hi".into(),
        model: None,
        cwd: std::env::temp_dir(),
    }
}

async fn collect(mut handle: sessions::SessionHandle) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(ev) = handle.events.recv().await {
        events.push(ev);
    }
    events
}

#[tokio::test]
async fn streams_events_from_process_in_order() {
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude/basic_session.jsonl");
    let script =
        std::fs::read_to_string(&fixture_path).expect("failed to read basic_session.jsonl fixture");
    let adapter = ScriptedAdapter {
        provider: Provider::Claude,
        script,
        delay_secs: 0,
    };
    let handle = sessions::spawn(Arc::new(adapter), req()).unwrap();
    let events = collect(handle).await;
    assert!(matches!(
        events.first(),
        Some(AgentEvent::SessionStarted { .. })
    ));
    assert!(matches!(events.last(), Some(AgentEvent::Completed { .. })));
}

#[tokio::test]
async fn nonzero_exit_emits_failed_event() {
    let handle = sessions::spawn(Arc::new(CrashingAdapter), req()).unwrap();
    let events = collect(handle).await;
    assert!(matches!(events.last(), Some(AgentEvent::Failed { error }) if error.contains("3")));
}

#[tokio::test]
async fn nonzero_exit_includes_stderr_tail() {
    struct StderrCrashingAdapter;
    impl Adapter for StderrCrashingAdapter {
        fn provider(&self) -> Provider {
            Provider::Claude
        }
        fn cli_binary(&self) -> &'static str {
            "sh"
        }
        fn build_command(&self, _req: &RunRequest) -> tokio::process::Command {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c").arg("echo boom >&2; exit 3");
            cmd
        }
    }
    let handle = sessions::spawn(Arc::new(StderrCrashingAdapter), req()).unwrap();
    let events = collect(handle).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Failed { error }) if error.contains("3") && error.contains("boom")
    ));
}

#[tokio::test]
async fn large_stderr_does_not_deadlock() {
    /// Writes ~200KB to stderr (well past the ~64KB pipe buffer) before
    /// printing "done" on stdout — deadlocks unless stderr is drained.
    struct NoisyStderrAdapter;
    impl Adapter for NoisyStderrAdapter {
        fn provider(&self) -> Provider {
            Provider::Claude
        }
        fn cli_binary(&self) -> &'static str {
            "sh"
        }
        fn build_command(&self, _req: &RunRequest) -> tokio::process::Command {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c")
                .arg("yes x | head -c 200000 >&2; echo done; exit 0");
            cmd
        }
        fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
            if line == "done" {
                vec![AgentEvent::Completed { result: None }]
            } else {
                Vec::new()
            }
        }
    }
    let handle = sessions::spawn(Arc::new(NoisyStderrAdapter), req()).unwrap();
    let events = tokio::time::timeout(std::time::Duration::from_secs(10), collect(handle))
        .await
        .expect("session deadlocked: stderr was not drained");
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::Completed { .. })));
}

#[tokio::test]
async fn missing_binary_returns_spawn_error() {
    struct MissingBinaryAdapter;
    impl Adapter for MissingBinaryAdapter {
        fn provider(&self) -> Provider {
            Provider::Claude
        }
        fn cli_binary(&self) -> &'static str {
            "definitely-not-a-real-binary-xyz"
        }
        fn build_command(&self, _req: &RunRequest) -> tokio::process::Command {
            tokio::process::Command::new("definitely-not-a-real-binary-xyz")
        }
    }
    let err = sessions::spawn(Arc::new(MissingBinaryAdapter), req()).unwrap_err();
    assert!(err.to_string().contains("definitely-not-a-real-binary-xyz"));
}
