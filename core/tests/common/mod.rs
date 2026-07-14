//! Shared test helpers. `ScriptedAdapter` fakes a CLI by `cat`-ing a given
//! claude-stream-json script through a real child process, so session
//! spawning/streaming is exercised end-to-end without spending any quota.

use consilium::adapters::{
    claude::ClaudeAdapter, codex::CodexAdapter, gemini::GeminiAdapter, grok::GrokAdapter, Adapter,
    FailureKind, RunRequest,
};
use consilium::event::{AgentEvent, Provider};
use std::sync::{Arc, Mutex};

/// Creates a real, local Git repository with one committed `base.txt` file.
/// Identity is supplied per command so the fixture never reads global Git
/// configuration and never performs network access.
#[allow(dead_code)]
pub fn committed_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("base.txt"), "base\n").unwrap();
    git(dir.path(), &["add", "--", "base.txt"]);
    commit(dir.path(), "base");
    dir
}

#[allow(dead_code)]
pub fn git(cwd: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[allow(dead_code)]
pub fn git_output(cwd: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[allow(dead_code)]
pub fn commit(cwd: &std::path::Path, message: &str) {
    git(
        cwd,
        &[
            "-c",
            "user.name=Consilium Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            message,
        ],
    );
}

// Each integration-test binary compiles its own copy of this module and uses a
// different subset of helpers — suppress per-binary dead_code noise.
#[allow(dead_code)]
pub struct ScriptedAdapter {
    pub provider: Provider,
    /// Raw claude-format stream-json lines the fake CLI will emit.
    pub script: String,
    /// Optional delay (seconds) before emitting — for timeout tests.
    pub delay_secs: u64,
    /// Shell snippet prepended to the heredoc script (runs first in the child
    /// process cwd). A fake worker can mutate a temp git repo here before
    /// reporting success — conduct tests exercise real change capture at zero
    /// quota cost.
    pub pre_script: String,
}

#[allow(dead_code)]
impl ScriptedAdapter {
    pub fn ok_with_text(provider: Provider, text: &str) -> Self {
        let script = format!(
            r#"{{"type":"system","subtype":"init","session_id":"scripted","model":"fake","tools":[]}}
{{"type":"assistant","message":{{"id":"m1","role":"assistant","content":[{{"type":"text","text":{text_json}}}]}},"session_id":"scripted"}}
{{"type":"result","subtype":"success","is_error":false,"result":{text_json},"session_id":"scripted","usage":{{"input_tokens":10,"output_tokens":5}}}}"#,
            text_json = serde_json::to_string(text).unwrap()
        );
        Self {
            provider,
            script,
            delay_secs: 0,
            pre_script: String::new(),
        }
    }

    /// Like [`ok_with_text`](Self::ok_with_text) but emits NO `usage` field, so
    /// no `Usage` event is produced — mirrors a CLI (e.g. Gemini via `agy`) that
    /// reports no token usage, forcing the runner's estimate fallback.
    pub fn ok_with_text_no_usage(provider: Provider, text: &str) -> Self {
        let script = format!(
            r#"{{"type":"system","subtype":"init","session_id":"scripted","model":"fake","tools":[]}}
{{"type":"assistant","message":{{"id":"m1","role":"assistant","content":[{{"type":"text","text":{text_json}}}]}},"session_id":"scripted"}}
{{"type":"result","subtype":"success","is_error":false,"result":{text_json},"session_id":"scripted"}}"#,
            text_json = serde_json::to_string(text).unwrap()
        );
        Self {
            provider,
            script,
            delay_secs: 0,
            pre_script: String::new(),
        }
    }

    pub fn failing(provider: Provider, error: &str) -> Self {
        let script = format!(
            r#"{{"type":"result","subtype":"error","is_error":true,"result":{e},"session_id":"scripted"}}"#,
            e = serde_json::to_string(error).unwrap()
        );
        Self {
            provider,
            script,
            delay_secs: 0,
            pre_script: String::new(),
        }
    }
}

impl Adapter for ScriptedAdapter {
    fn provider(&self) -> Provider {
        self.provider
    }
    fn cli_binary(&self) -> &'static str {
        "sh"
    }
    /// Delegates to the real adapter for this provider so tests exercise real
    /// failure classification rather than the trait default (Transient).
    fn classify_failure(&self, error: &str) -> FailureKind {
        match self.provider {
            Provider::Claude => ClaudeAdapter.classify_failure(error),
            Provider::Codex => CodexAdapter.classify_failure(error),
            Provider::Gemini => GeminiAdapter.classify_failure(error),
            Provider::Grok => GrokAdapter.classify_failure(error),
        }
    }
    fn build_command(&self, req: &RunRequest) -> tokio::process::Command {
        debug_assert!(
            !self.script.lines().any(|l| l == "CONSILIUM_EOF"),
            "ScriptedAdapter: script contains the literal heredoc delimiter 'CONSILIUM_EOF' as a standalone line; output will be truncated"
        );
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(format!(
            "{}\nsleep {}; cat <<'CONSILIUM_EOF'\n{}\nCONSILIUM_EOF",
            self.pre_script, self.delay_secs, self.script
        ));
        cmd.current_dir(&req.cwd);
        cmd
    }
    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        ClaudeAdapter.parse_line(line)
    }
}

/// Wraps a sequence of [`ScriptedAdapter`] steps, advancing one step per
/// `build_command` call (clamped to the last step if over-called). Lets one
/// logical role (e.g. the conductor) return different scripted responses across
/// sequential invocations — plan → verdict → verdict … — without quota.
#[allow(dead_code)]
pub struct SequencedAdapter {
    pub provider: Provider,
    pub steps: Vec<ScriptedAdapter>,
    cursor: std::sync::atomic::AtomicUsize,
}

#[allow(dead_code)]
impl SequencedAdapter {
    pub fn new(provider: Provider, steps: Vec<ScriptedAdapter>) -> Self {
        Self {
            provider,
            steps,
            cursor: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl Adapter for SequencedAdapter {
    fn provider(&self) -> Provider {
        self.provider
    }
    fn cli_binary(&self) -> &'static str {
        "sh"
    }
    fn build_command(&self, req: &RunRequest) -> tokio::process::Command {
        debug_assert!(
            !self.steps.is_empty(),
            "SequencedAdapter: steps must be non-empty"
        );
        let i = self
            .cursor
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .min(self.steps.len().saturating_sub(1)); // clamp: repeat last step if over-called
        self.steps[i].build_command(req)
    }
    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        ClaudeAdapter.parse_line(line)
    }
}

/// An adapter whose `build_command` points at a binary that does not exist, so
/// `sessions::spawn` returns an `Err` at LAUNCH (before any event is emitted).
/// Exercises the failover engine's spawn-error path: a launch failure must
/// demote to the next rung WITHOUT aborting the ladder and WITHOUT marking the
/// model dead. `classify_failure` is left as the trait default (Transient).
#[allow(dead_code)]
pub struct SpawnFailAdapter {
    pub provider: Provider,
}

#[allow(dead_code)]
impl SpawnFailAdapter {
    pub fn new(provider: Provider) -> Self {
        Self { provider }
    }
}

impl Adapter for SpawnFailAdapter {
    fn provider(&self) -> Provider {
        self.provider
    }
    fn cli_binary(&self) -> &'static str {
        "consilium-no-such-binary-xyz9"
    }
    fn build_command(&self, _req: &RunRequest) -> tokio::process::Command {
        tokio::process::Command::new("consilium-no-such-binary-xyz9")
    }
    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        ClaudeAdapter.parse_line(line)
    }
}

/// Wraps an inner [`ScriptedAdapter`] and records each `build_command` call's
/// prompt, advisory flag, and write flag into a shared log. Lets integration
/// tests assert what prompts were fed to a role (e.g. that a supervisor note
/// reached the conductor's evaluation prompt).
///
/// `parse_line` delegates to `ClaudeAdapter` as usual.
#[allow(dead_code)]
pub struct RecordingAdapter {
    pub provider: Provider,
    inner: ScriptedAdapter,
    /// Appended entries: (prompt, advisory, write) per build_command call.
    pub log: Arc<Mutex<Vec<(String, bool, bool)>>>,
}

#[allow(dead_code)]
impl RecordingAdapter {
    pub fn new(inner: ScriptedAdapter, log: Arc<Mutex<Vec<(String, bool, bool)>>>) -> Self {
        Self {
            provider: inner.provider,
            inner,
            log,
        }
    }
}

impl Adapter for RecordingAdapter {
    fn provider(&self) -> Provider {
        self.provider
    }
    fn cli_binary(&self) -> &'static str {
        "sh"
    }
    fn build_command(&self, req: &RunRequest) -> tokio::process::Command {
        {
            let mut guard = self.log.lock().unwrap();
            guard.push((req.prompt.clone(), req.advisory, req.write));
        }
        self.inner.build_command(req)
    }
    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        ClaudeAdapter.parse_line(line)
    }
}

/// Like [`SequencedAdapter`] but ALSO records each call's (prompt, advisory,
/// write) into a shared log — so a test can sequence a role's responses AND
/// assert what prompt each invocation received (e.g. that subtask N's evaluation
/// prompt carries the prior subtasks' ledger, or attempt N's prompt carries the
/// prior attempts' history). Indices in the log line up with call order.
#[allow(dead_code)]
pub struct RecordingSequenced {
    pub provider: Provider,
    pub steps: Vec<ScriptedAdapter>,
    pub log: Arc<Mutex<Vec<(String, bool, bool)>>>,
    cursor: std::sync::atomic::AtomicUsize,
}

#[allow(dead_code)]
impl RecordingSequenced {
    pub fn new(
        provider: Provider,
        steps: Vec<ScriptedAdapter>,
        log: Arc<Mutex<Vec<(String, bool, bool)>>>,
    ) -> Self {
        Self {
            provider,
            steps,
            log,
            cursor: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl Adapter for RecordingSequenced {
    fn provider(&self) -> Provider {
        self.provider
    }
    fn cli_binary(&self) -> &'static str {
        "sh"
    }
    fn build_command(&self, req: &RunRequest) -> tokio::process::Command {
        {
            let mut guard = self.log.lock().unwrap();
            guard.push((req.prompt.clone(), req.advisory, req.write));
        }
        let i = self
            .cursor
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .min(self.steps.len().saturating_sub(1));
        self.steps[i].build_command(req)
    }
    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        ClaudeAdapter.parse_line(line)
    }
}
