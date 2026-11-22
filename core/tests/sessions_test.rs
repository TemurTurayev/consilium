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
        advisory: false,
        write: false,
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
        pre_script: String::new(),
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

/// The advisory+write combination is contradictory (deliberation never writes)
/// and must be rejected by the spawn invariant in EVERY build (hard bail, not a
/// debug_assert — release binaries must enforce it too).
#[tokio::test]
async fn advisory_plus_write_combination_is_rejected() {
    let adapter = Arc::new(CrashingAdapter);
    let bad = RunRequest {
        prompt: "x".into(),
        model: None,
        cwd: std::env::temp_dir(),
        advisory: true,
        write: true,
    };
    let err = sessions::spawn(adapter, bad).unwrap_err();
    assert!(err.to_string().contains("contradictory RunRequest"));
}

/// Mimics `agy`: a non-streaming CLI whose adapter reports "produced no output"
/// when stdout is empty (delegates to the real `GeminiAdapter::parse_final`).
struct NonStreamingCli {
    script: &'static str,
}

impl Adapter for NonStreamingCli {
    fn provider(&self) -> Provider {
        Provider::Gemini
    }
    fn cli_binary(&self) -> &'static str {
        "sh"
    }
    fn build_command(&self, _req: &RunRequest) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(self.script);
        cmd
    }
    fn parse_final(&self, full_output: &str) -> Vec<AgentEvent> {
        consilium::adapters::gemini::GeminiAdapter.parse_final(full_output)
    }
}

fn first_terminal(events: &[AgentEvent]) -> Option<&AgentEvent> {
    events
        .iter()
        .find(|e| matches!(e, AgentEvent::Failed { .. } | AgentEvent::Completed { .. }))
}

/// Regression (agy 1.5, 2026-11): an unknown `--model` makes agy print the real
/// reason on stderr, leave stdout empty and exit 1. The first terminal event is
/// authoritative, so it must carry the CLI's own message — not the adapter's
/// generic "produced no output" fallback, which hid the cause and made the
/// failure look transient instead of "model unavailable".
#[tokio::test]
async fn nonzero_exit_stderr_outranks_the_empty_output_fallback() {
    let adapter = NonStreamingCli {
        script: "echo 'error: invalid model selection' >&2; exit 1",
    };
    let events = collect(sessions::spawn(Arc::new(adapter), req()).unwrap()).await;
    assert!(
        matches!(
            first_terminal(&events),
            Some(AgentEvent::Failed { error }) if error.contains("invalid model selection")
        ),
        "first terminal event must carry stderr, got: {events:?}"
    );
}

/// A CLI that dies mid-answer (non-zero exit after partial stdout) has failed:
/// the partial text must not be reported as a completed run.
#[tokio::test]
async fn nonzero_exit_outranks_partial_output() {
    let adapter = NonStreamingCli {
        script: "echo 'half an answer'; echo 'stream timed out' >&2; exit 1",
    };
    let events = collect(sessions::spawn(Arc::new(adapter), req()).unwrap()).await;
    assert!(
        matches!(
            first_terminal(&events),
            Some(AgentEvent::Failed { error }) if error.contains("stream timed out")
        ),
        "a crashed run must fail, got: {events:?}"
    );
    // The partial text is still surfaced for diagnostics.
    assert!(events
        .iter()
        .any(|e| matches!(e, AgentEvent::Message { text } if text == "half an answer")));
}

/// Exit 0 with empty stdout keeps the adapter's own explanation.
#[tokio::test]
async fn clean_exit_with_no_output_keeps_the_adapter_failure() {
    let adapter = NonStreamingCli { script: "exit 0" };
    let events = collect(sessions::spawn(Arc::new(adapter), req()).unwrap()).await;
    assert!(matches!(
        first_terminal(&events),
        Some(AgentEvent::Failed { error }) if error.contains("produced no output")
    ));
}

/// Fake CLI that prints one environment variable — the credential its adapter
/// declares — so a test can see what the spawned process was given.
struct CredentialEchoAdapter;

const ECHO_CREDENTIAL: &str = "CONSILIUM_TEST_SAVED_CREDENTIAL";

impl Adapter for CredentialEchoAdapter {
    fn provider(&self) -> Provider {
        Provider::Claude
    }
    fn cli_binary(&self) -> &'static str {
        "sh"
    }
    fn credential_env(&self) -> &'static [&'static str] {
        &[ECHO_CREDENTIAL]
    }
    fn build_command(&self, _req: &RunRequest) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(format!("printf '%s' \"${{{ECHO_CREDENTIAL}:-unset}}\""));
        cmd
    }
    fn parse_final(&self, full_output: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::Completed {
            result: Some(full_output.trim().to_string()),
        }]
    }
}

fn completed_text(events: &[AgentEvent]) -> Option<&str> {
    events.iter().find_map(|event| match event {
        AgentEvent::Completed { result } => result.as_deref(),
        _ => None,
    })
}

/// Regression (2026-11-22): the MCP server inside the Claude desktop app is not
/// started from a shell, so it never had the `CLAUDE_CODE_OAUTH_TOKEN` a
/// terminal gets from the shell profile, and every Claude role failed with a
/// 401. A provider CLI must be handed the saved credential it is missing.
#[tokio::test]
async fn a_spawned_cli_receives_its_saved_credential() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = dir.path().join("secrets.env");
    std::fs::write(
        &secrets,
        format!("export {ECHO_CREDENTIAL}=from-the-file\n"),
    )
    .unwrap();

    let handle =
        sessions::spawn_with_secrets(Arc::new(CredentialEchoAdapter), req(), Some(&secrets))
            .unwrap();
    let events = collect(handle).await;

    assert_eq!(completed_text(&events), Some("from-the-file"));
}

#[tokio::test]
async fn a_cli_without_a_secrets_file_is_spawned_unchanged() {
    let handle =
        sessions::spawn_with_secrets(Arc::new(CredentialEchoAdapter), req(), None).unwrap();
    let events = collect(handle).await;

    assert_eq!(completed_text(&events), Some("unset"));
}

/// A CLI whose adapter names a sign-in prompt, as `agy` and `grok` do.
struct NamesSignInPrompt {
    script: String,
}

const SIGN_IN_PROMPT: &str = "Please sign in:";

impl Adapter for NamesSignInPrompt {
    fn provider(&self) -> Provider {
        Provider::Gemini
    }
    fn cli_binary(&self) -> &'static str {
        "sh"
    }
    fn sign_in_prompts(&self) -> &'static [&'static str] {
        &[SIGN_IN_PROMPT]
    }
    fn build_command(&self, _req: &RunRequest) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(&self.script);
        cmd
    }
    fn parse_final(&self, full_output: &str) -> Vec<AgentEvent> {
        vec![AgentEvent::Completed {
            result: Some(full_output.trim().to_string()),
        }]
    }
}

fn is_running(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Whether the process whose pid `pid_file` holds is gone within two seconds
/// (a killed process that was orphaned takes a moment to be collected).
async fn is_gone(pid_file: &std::path::Path) -> bool {
    let pid = std::fs::read_to_string(pid_file).expect("the fake CLI wrote its pid");
    for _ in 0..40 {
        if !is_running(pid.trim()) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    false
}

/// Regression (2026-11-22): a signed-out agy waited 60 s for a browser sign-in
/// and a signed-out grok 0.2.87 waited for ever, each holding its seat in the
/// run the whole time. The run must end as soon as the CLI asks for a sign-in
/// — as a failure that says so, without the sign-in link — and the CLI must
/// be gone with everything it started.
#[tokio::test]
async fn a_cli_waiting_for_a_sign_in_is_stopped_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let (cli_pid, helper_pid) = (dir.path().join("cli"), dir.path().join("helper"));
    // Mimics a signed-out agy: start a helper, ask on stderr, then wait for a
    // browser sign-in that a headless run can never complete.
    let adapter = NamesSignInPrompt {
        script: format!(
            "sleep 60 & echo $! > '{}'; echo $$ > '{}'; echo '{SIGN_IN_PROMPT} https://example.test/secret-code' >&2; wait",
            helper_pid.display(),
            cli_pid.display()
        ),
    };

    let handle = sessions::spawn(Arc::new(adapter), req()).unwrap();
    let events = tokio::time::timeout(std::time::Duration::from_secs(10), collect(handle))
        .await
        .expect("the session waited for a sign-in that cannot happen");

    let Some(AgentEvent::Failed { error }) = events.last() else {
        panic!("expected a failure, got {events:?}");
    };
    assert!(error.contains("sh is not signed in"), "{error}");
    assert!(error.contains(SIGN_IN_PROMPT), "{error}");
    assert!(!error.contains("secret-code"), "{error}");

    assert!(is_gone(&cli_pid).await, "the CLI was left running");
    assert!(
        is_gone(&helper_pid).await,
        "the CLI's helper was left running"
    );
}

/// A CLI can close its stdout and still be waiting for a sign-in: the end of
/// stdout is not the end of the watch.
#[tokio::test]
async fn a_cli_that_closed_stdout_before_asking_is_stopped_too() {
    let adapter = NamesSignInPrompt {
        script: format!(
            "exec 1>&-; sleep 0.3; echo '{SIGN_IN_PROMPT} https://example.test' >&2; exec sleep 60"
        ),
    };

    let handle = sessions::spawn(Arc::new(adapter), req()).unwrap();
    let events = tokio::time::timeout(std::time::Duration::from_secs(10), collect(handle))
        .await
        .expect("the session waited for a sign-in that cannot happen");

    assert!(
        matches!(
            first_terminal(&events),
            Some(AgentEvent::Failed { error }) if error.contains("sh is not signed in")
        ),
        "got: {events:?}"
    );
}

/// The same words on stderr mean nothing for a CLI that does not name them as
/// its sign-in prompt: the run goes on to its own end.
#[tokio::test]
async fn stderr_text_is_no_sign_in_prompt_unless_the_adapter_names_it() {
    let adapter = NonStreamingCli {
        script: "echo 'Please sign in: just a log line' >&2; echo 'the answer'",
    };
    let events = collect(sessions::spawn(Arc::new(adapter), req()).unwrap()).await;
    assert!(matches!(
        first_terminal(&events),
        Some(AgentEvent::Completed { result: Some(text) }) if text == "the answer"
    ));
}

/// A CLI asks for a sign-in before it says anything else. The same words deep
/// in a working CLI's log — a worker editing this very code, say — are not a
/// prompt, and must not cost the run.
#[tokio::test]
async fn a_sign_in_prompt_late_in_stderr_does_not_stop_a_working_cli() {
    let adapter = NamesSignInPrompt {
        script: format!(
            "yes log | head -c 6000 >&2; echo '{SIGN_IN_PROMPT} quoted from a file' >&2; sleep 0.3; echo 'the answer'"
        ),
    };
    let events = collect(sessions::spawn(Arc::new(adapter), req()).unwrap()).await;
    assert!(
        matches!(
            first_terminal(&events),
            Some(AgentEvent::Completed { result: Some(text) }) if text == "the answer"
        ),
        "got: {events:?}"
    );
}
