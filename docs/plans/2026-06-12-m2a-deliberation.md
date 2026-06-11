# Consilium M2a: Deliberation (council + review) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two working commands on top of the M1 engine: `consilium council "<question>"` (N agents answer independently → anonymized cross-review → chairman synthesis) and `consilium review` (one agent audits a git diff), with full JSON transcripts and quota recording.

**Architecture:** New `orchestrator/` module family. `runner.rs` collects a session to completion (the one bridge from streaming to request/response). `roles.rs` maps config roles to adapters. `prompts.rs` holds all prompt templates. `council.rs`/`review.rs` are pure orchestration logic, unit-tested with scripted fake adapters (zero quota spent in tests). Transcripts are human-readable JSON files under `~/.consilium/runs/`.

**Tech Stack:** Rust (existing M1 stack: tokio, serde, anyhow, rusqlite). No new dependencies except `rand` (anonymization shuffle).

**Repo:** `/Users/temur/Desktop/Claude/consilium`, branch `m2a-deliberation` off `main`. Baseline: 37 tests green.

**Scope notes (decided during planning):**
- `conduct`, `auto`, `supervisor`, quota-aware routing → **M2b** (separate plan).
- Review arbiter (chairman on dispute) activates in M2b when conduct produces author/reviewer pairs; M2a review has a single reviewer, no dispute path.
- Worker rework = stateless re-prompting (new one-shot session with a self-contained context packet), NOT `--resume`: uniform across all three CLIs.
- Role `effort` is NOT yet translated to CLI flags (flag names unverified against real CLIs) — carry a TODO, verify in M2b.

---

### Task 1: Scripted test adapter (shared test helper)

**Files:**
- Create: `core/tests/common/mod.rs`
- Modify: `core/tests/sessions_test.rs` (reuse helper, delete local FakeAdapter)

The pattern "fake CLI = `sh -c 'cat <<EOF'` emitting claude-format lines, parse delegates to ClaudeAdapter" already exists privately in sessions_test.rs. Orchestrator tests need it too — extract it.

- [ ] **Step 1: Create `core/tests/common/mod.rs`**

```rust
//! Shared test helpers. `ScriptedAdapter` fakes a CLI by `cat`-ing a given
//! claude-stream-json script through a real child process, so session
//! spawning/streaming is exercised end-to-end without spending any quota.

use consilium::adapters::{claude::ClaudeAdapter, Adapter, RunRequest};
use consilium::event::{AgentEvent, Provider};

// Each integration-test binary compiles its own copy of this module and uses a
// different subset of helpers — suppress per-binary dead_code noise.
#[allow(dead_code)]
pub struct ScriptedAdapter {
    pub provider: Provider,
    /// Raw claude-format stream-json lines the fake CLI will emit.
    pub script: String,
    /// Optional delay (seconds) before emitting — for timeout tests.
    pub delay_secs: u64,
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
        Self { provider, script, delay_secs: 0 }
    }

    pub fn failing(provider: Provider, error: &str) -> Self {
        let script = format!(
            r#"{{"type":"result","subtype":"error","is_error":true,"result":{e},"session_id":"scripted"}}"#,
            e = serde_json::to_string(error).unwrap()
        );
        Self { provider, script, delay_secs: 0 }
    }
}

impl Adapter for ScriptedAdapter {
    fn provider(&self) -> Provider {
        self.provider
    }
    fn cli_binary(&self) -> &'static str {
        "sh"
    }
    fn build_command(&self, _req: &RunRequest) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(format!(
            "sleep {}; cat <<'CONSILIUM_EOF'\n{}\nCONSILIUM_EOF",
            self.delay_secs, self.script
        ));
        cmd
    }
    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        ClaudeAdapter.parse_line(line)
    }
}
```

- [ ] **Step 2: Refactor `core/tests/sessions_test.rs`** — add `mod common;` and `use common::ScriptedAdapter;`. Replace the local `FakeAdapter { fixture }` usage in `streams_events_from_process_in_order` with reading the fixture file into a string and using `ScriptedAdapter { provider: Provider::Claude, script: <fixture contents read via std::fs::read_to_string>, delay_secs: 0 }`. Keep `CrashingAdapter`, `MissingBinaryAdapter`, and the stderr tests as-is (they test other shapes). Delete `core/tests/fake_cli_output.jsonl` and its sync-warning doc comment; read `core/tests/fixtures/claude/basic_session.jsonl` directly instead.

- [ ] **Step 3: Run full suite, verify green**

Run: `source "$HOME/.cargo/env" && cargo test`
Expected: 37 passed (no count change — pure refactor)

- [ ] **Step 4: `cargo fmt --all && cargo clippy --all-targets -- -D warnings`, commit**

```bash
git -C /Users/temur/Desktop/Claude/consilium add -A
git -C /Users/temur/Desktop/Claude/consilium commit -m "test: extract ScriptedAdapter shared helper"
```

### Task 2: runner — collect a session to completion (TDD)

**Files:**
- Create: `core/src/orchestrator/mod.rs`
- Create: `core/src/orchestrator/runner.rs`
- Create: `core/tests/runner_test.rs`
- Modify: `core/src/lib.rs` (add `pub mod orchestrator;`)

- [ ] **Step 1: Write failing integration test** `core/tests/runner_test.rs`:

```rust
mod common;

use common::ScriptedAdapter;
use consilium::adapters::RunRequest;
use consilium::event::Provider;
use consilium::orchestrator::runner::{run_to_completion, RunStatus};
use consilium::quota::QuotaStore;
use std::sync::Arc;
use std::time::Duration;

fn req() -> RunRequest {
    RunRequest { prompt: "q".into(), model: None, cwd: std::env::temp_dir() }
}

#[tokio::test]
async fn collects_final_text_and_records_usage() {
    let store = QuotaStore::open_in_memory().unwrap();
    let adapter = Arc::new(ScriptedAdapter::ok_with_text(Provider::Gemini, "the answer"));
    let outcome = run_to_completion(adapter, req(), &store, Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(outcome.final_text, "the answer");
    assert!(matches!(outcome.status, RunStatus::Completed));
    let (input, output) = store.totals_since(Provider::Gemini, 0).unwrap();
    assert_eq!((input, output), (10, 5));
}

#[tokio::test]
async fn failed_event_yields_failed_status() {
    let store = QuotaStore::open_in_memory().unwrap();
    let adapter = Arc::new(ScriptedAdapter::failing(Provider::Codex, "limit reached"));
    let outcome = run_to_completion(adapter, req(), &store, Duration::from_secs(30))
        .await
        .unwrap();
    assert!(matches!(&outcome.status, RunStatus::Failed(e) if e.contains("limit reached")));
}

#[tokio::test]
async fn timeout_yields_timedout_status() {
    let store = QuotaStore::open_in_memory().unwrap();
    let adapter = Arc::new(ScriptedAdapter {
        provider: Provider::Gemini,
        script: String::new(),
        delay_secs: 30,
    });
    let outcome = run_to_completion(adapter, req(), &store, Duration::from_millis(200))
        .await
        .unwrap();
    assert!(matches!(outcome.status, RunStatus::TimedOut));
}
```

- [ ] **Step 2: Run, verify COMPILE ERROR (RED)**

Run: `cargo test --test runner_test`

- [ ] **Step 3: Implement.** `core/src/orchestrator/mod.rs`:

```rust
pub mod runner;
// pub mod roles;      // Task 3
// pub mod prompts;    // Task 4
// pub mod transcript; // Task 5
// pub mod council;    // Task 6
// pub mod review;     // Task 7
```

`core/src/orchestrator/runner.rs`:

```rust
use crate::adapters::{Adapter, RunRequest};
use crate::event::AgentEvent;
use crate::quota::QuotaStore;
use crate::sessions;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum RunStatus {
    Completed,
    Failed(String),
    TimedOut,
}

#[derive(Debug)]
pub struct RunOutcome {
    pub session_id: String,
    /// Completed.result if present, else the last Message text, else empty.
    pub final_text: String,
    pub events: Vec<AgentEvent>,
    pub status: RunStatus,
}

/// Drives one agent session to completion: collects all events, records Usage
/// into the quota store, derives the final text, and applies a hard timeout.
/// First terminal event (Completed/Failed) is authoritative (see sessions.rs
/// design note); a timeout abandons the stream (child is orphaned — M1 policy).
pub async fn run_to_completion(
    adapter: Arc<dyn Adapter>,
    req: RunRequest,
    quota: &QuotaStore,
    timeout: Duration,
) -> anyhow::Result<RunOutcome> {
    let provider = adapter.provider();
    let mut handle = sessions::spawn(adapter, req)?;
    let session_id = handle.id.clone();

    let mut events: Vec<AgentEvent> = Vec::new();
    let mut status: Option<RunStatus> = None;

    let collect = async {
        while let Some(ev) = handle.events.recv().await {
            match &ev {
                AgentEvent::Usage { input_tokens, output_tokens } => {
                    quota.record(provider, *input_tokens, *output_tokens)?;
                }
                AgentEvent::Completed { .. } if status.is_none() => {
                    status = Some(RunStatus::Completed);
                }
                AgentEvent::Failed { error } if status.is_none() => {
                    status = Some(RunStatus::Failed(error.clone()));
                }
                _ => {}
            }
            events.push(ev);
        }
        anyhow::Ok(())
    };

    let timed_out = tokio::time::timeout(timeout, collect).await.is_err();

    let status = if timed_out {
        RunStatus::TimedOut
    } else {
        status.unwrap_or_else(|| RunStatus::Failed("stream ended without terminal event".into()))
    };

    let final_text = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Completed { result: Some(r) } => Some(r.clone()),
            _ => None,
        })
        .or_else(|| {
            events.iter().rev().find_map(|e| match e {
                AgentEvent::Message { text } => Some(text.clone()),
                _ => None,
            })
        })
        .unwrap_or_default();

    Ok(RunOutcome { session_id, final_text, events, status })
}
```

Borrow-checker note: `status` is captured by the `collect` future and read after the await — if the compiler objects, restructure with a local enum inside the future returning `(Vec<AgentEvent>, Option<RunStatus>)` from the async block instead of mutating captured locals. Either shape is acceptable; behavior is the contract.

- [ ] **Step 4: GREEN + gates**

Run: `cargo test` → 40 total. `cargo fmt --all && cargo clippy --all-targets -- -D warnings`.

- [ ] **Step 5: Commit** `feat: runner collects sessions to completion with timeout and quota recording`

### Task 3: roles — config → adapter factory (TDD)

**Files:**
- Create: `core/src/orchestrator/roles.rs` (uncomment in mod.rs)

- [ ] **Step 1: Failing unit tests** (bottom of roles.rs):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RoleConfig;
    use crate::event::Provider;

    fn role(provider: Provider, model: &str) -> RoleConfig {
        serde_json::from_value(serde_json::json!({
            "provider": provider.as_str(),
            "model": model
        }))
        .unwrap()
    }

    #[test]
    fn adapter_for_maps_each_provider() {
        assert_eq!(adapter_for(&role(Provider::Claude, "sonnet")).provider(), Provider::Claude);
        assert_eq!(adapter_for(&role(Provider::Codex, "gpt-5.4")).provider(), Provider::Codex);
        assert_eq!(adapter_for(&role(Provider::Gemini, "gemini-3-pro")).provider(), Provider::Gemini);
    }

    #[test]
    fn request_for_carries_model_and_prompt() {
        let r = request_for(&role(Provider::Codex, "gpt-5.4"), "do it".into(), std::env::temp_dir());
        assert_eq!(r.model.as_deref(), Some("gpt-5.4"));
        assert_eq!(r.prompt, "do it");
    }
}
```

- [ ] **Step 2: RED**, then implement:

```rust
use crate::adapters::{claude::ClaudeAdapter, codex::CodexAdapter, gemini::GeminiAdapter, Adapter, RunRequest};
use crate::config::RoleConfig;
use crate::event::Provider;
use std::path::PathBuf;
use std::sync::Arc;

pub fn adapter_for(role: &RoleConfig) -> Arc<dyn Adapter> {
    match role.provider {
        Provider::Claude => Arc::new(ClaudeAdapter),
        Provider::Codex => Arc::new(CodexAdapter),
        Provider::Gemini => Arc::new(GeminiAdapter),
    }
}

/// Builds the RunRequest for a role. `effort` is intentionally NOT applied yet:
/// per-CLI effort flags are unverified — TODO(M2b): map after checking real CLIs.
pub fn request_for(role: &RoleConfig, prompt: String, cwd: PathBuf) -> RunRequest {
    RunRequest { prompt, model: Some(role.model.clone()), cwd }
}
```

- [ ] **Step 3: GREEN (42 tests), fmt+clippy, commit** `feat: role-to-adapter factory`

### Task 4: prompts — templates for council and review (TDD)

**Files:**
- Create: `core/src/orchestrator/prompts.rs` (uncomment in mod.rs)

- [ ] **Step 1: Failing tests:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_prompt_contains_question() {
        let p = council_answer("Why is the sky blue?");
        assert!(p.contains("Why is the sky blue?"));
        assert!(p.contains("independent expert"));
    }

    #[test]
    fn review_prompt_lists_anonymized_answers() {
        let p = council_review("Q?", &[("A", "ans1"), ("B", "ans2")]);
        assert!(p.contains("Agent A"));
        assert!(p.contains("ans2"));
        assert!(p.contains(r#""scores""#)); // demands the JSON contract
    }

    #[test]
    fn synthesis_prompt_includes_answers_and_reviews() {
        let p = council_synthesis("Q?", &[("A", "ans1")], &["review text"]);
        assert!(p.contains("ans1"));
        assert!(p.contains("review text"));
        assert!(p.contains("final answer"));
    }

    #[test]
    fn diff_review_prompt_embeds_diff_and_contract() {
        let p = diff_review("--- a/x.rs\n+++ b/x.rs");
        assert!(p.contains("+++ b/x.rs"));
        assert!(p.contains(r#""findings""#));
    }
}
```

- [ ] **Step 2: RED**, then implement (exact template texts — these are product copy, keep them):

```rust
//! All prompt templates in one place. Templates demand strict JSON blocks so
//! downstream parsing is testable; parsers must still tolerate non-compliance.

pub fn council_answer(question: &str) -> String {
    format!(
        "You are one independent expert on a council. Answer the question below \
         thoroughly but concisely. Do not hedge across multiple options — commit \
         to the best answer and justify it.\n\nQuestion:\n{question}"
    )
}

pub fn council_review(question: &str, answers: &[(&str, &str)]) -> String {
    let mut body = String::new();
    for (label, text) in answers {
        body.push_str(&format!("\n--- Answer from Agent {label} ---\n{text}\n"));
    }
    format!(
        "You are reviewing anonymized answers from a council of AI agents (one of \
         them may be your own — judge it just as critically).\n\nQuestion:\n{question}\n{body}\n\
         Review each answer for correctness, depth, and practicality. Then output \
         EXACTLY one JSON code block in this format:\n```json\n{{\"scores\":[{{\"agent\":\"A\",\"score\":8,\"justification\":\"...\"}}]}}\n```\n\
         Score range 1-10. One entry per answer."
    )
}

pub fn council_synthesis(question: &str, answers: &[(&str, &str)], reviews: &[&str]) -> String {
    let mut answers_body = String::new();
    for (label, text) in answers {
        answers_body.push_str(&format!("\n--- Answer from Agent {label} ---\n{text}\n"));
    }
    let mut reviews_body = String::new();
    for (i, r) in reviews.iter().enumerate() {
        reviews_body.push_str(&format!("\n--- Review {} ---\n{r}\n", i + 1));
    }
    format!(
        "You are the chairman of an AI council. Below are the question, the \
         anonymized answers, and the cross-reviews. Synthesize the single best \
         final answer: take the strongest points, discard the weak ones, resolve \
         contradictions explicitly. Output the final answer only — no meta-commentary \
         about the process.\n\nQuestion:\n{question}\n{answers_body}{reviews_body}"
    )
}

pub fn diff_review(diff: &str) -> String {
    format!(
        "Review this diff for real problems: bugs, security issues, broken edge \
         cases, misleading naming. Do not invent style nitpicks. Then output EXACTLY \
         one JSON code block:\n```json\n{{\"findings\":[{{\"severity\":\"critical|important|minor\",\"file\":\"path\",\"description\":\"...\"}}]}}\n```\n\
         Empty findings array means the diff is clean.\n\nDiff:\n```diff\n{diff}\n```"
    )
}
```

- [ ] **Step 3: GREEN (46), fmt+clippy, commit** `feat: prompt templates for council and review`

### Task 5: transcript — JSON run records (TDD)

**Files:**
- Create: `core/src/orchestrator/transcript.rs` (uncomment in mod.rs)

- [ ] **Step 1: Failing tests:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_back_run_json() {
        let dir = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(dir.path().to_path_buf());
        let path = store
            .save("council", &serde_json::json!({"question": "q", "stage": 1}))
            .unwrap();
        assert!(path.starts_with(dir.path()));
        assert!(path.to_string_lossy().contains("council"));
        let raw = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["question"], "q");
    }

    #[test]
    fn run_ids_are_unique_and_sorted_by_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = TranscriptStore::new(dir.path().to_path_buf());
        let a = store.save("council", &serde_json::json!({})).unwrap();
        let b = store.save("council", &serde_json::json!({})).unwrap();
        assert_ne!(a, b);
    }
}
```

Note: `tempfile` is already a dev-dependency.

- [ ] **Step 2: RED**, then implement:

```rust
use std::path::{Path, PathBuf};

/// Human-readable JSON transcripts under `<base>/runs/<unix_nanos>-<kind>.json`.
/// Files, not SQLite: transcripts are for humans to read and diff; the M3
/// server can index them later.
pub struct TranscriptStore {
    base: PathBuf,
}

impl TranscriptStore {
    pub fn new(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn default_base() -> anyhow::Result<PathBuf> {
        let home = std::env::var("HOME")
            .map_err(|_| anyhow::anyhow!("$HOME is not set; cannot locate ~/.consilium"))?;
        Ok(Path::new(&home).join(".consilium"))
    }

    pub fn save(&self, kind: &str, payload: &serde_json::Value) -> anyhow::Result<PathBuf> {
        let dir = self.base.join("runs");
        std::fs::create_dir_all(&dir)?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before unix epoch")
            .as_nanos();
        let path = dir.join(format!("{nanos}-{kind}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(payload)?)?;
        Ok(path)
    }
}
```

- [ ] **Step 3: GREEN (48), fmt+clippy, commit** `feat: JSON transcript store`

### Task 6: council — three-stage deliberation (TDD)

**Files:**
- Create: `core/src/orchestrator/council.rs` (uncomment in mod.rs)
- Create: `core/tests/council_test.rs`

- [ ] **Step 1: Failing unit tests for score parsing** (bottom of council.rs):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_scores_block() {
        let text = "Thoughts...\n```json\n{\"scores\":[{\"agent\":\"A\",\"score\":8,\"justification\":\"solid\"}]}\n```\ndone";
        let scores = parse_scores(text).unwrap();
        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].agent, "A");
        assert_eq!(scores[0].score, 8);
    }

    #[test]
    fn parses_bare_json_without_fence() {
        let text = r#"{"scores":[{"agent":"B","score":3,"justification":"weak"}]}"#;
        let scores = parse_scores(text).unwrap();
        assert_eq!(scores[0].agent, "B");
    }

    #[test]
    fn malformed_output_yields_none() {
        assert!(parse_scores("no json here at all").is_none());
        assert!(parse_scores("```json\n{\"broken\": tru\n```").is_none());
    }
}
```

- [ ] **Step 2: Failing integration test** `core/tests/council_test.rs` (scripted, zero quota):

```rust
mod common;

use common::ScriptedAdapter;
use consilium::event::Provider;
use consilium::orchestrator::council::{run_council, CouncilMember};
use consilium::quota::QuotaStore;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn full_council_flow_with_scripted_members() {
    let store = QuotaStore::open_in_memory().unwrap();
    // Stage answers are plain text; stage-2 reviews return a JSON scores block;
    // ScriptedAdapter replays the same script for every call, so member answers
    // double as their review responses — parse_scores simply finds no JSON in
    // stage 2 for member 1 (None is tolerated by design).
    let members = vec![
        CouncilMember {
            label: "codex-worker".into(),
            adapter: Arc::new(ScriptedAdapter::ok_with_text(Provider::Codex, "use sqlite")),
            model: None,
        },
        CouncilMember {
            label: "gemini-worker".into(),
            adapter: Arc::new(ScriptedAdapter::ok_with_text(
                Provider::Gemini,
                "```json\n{\"scores\":[{\"agent\":\"A\",\"score\":7,\"justification\":\"ok\"}]}\n```",
            )),
            model: None,
        },
    ];
    let chairman = Arc::new(ScriptedAdapter::ok_with_text(Provider::Claude, "final: use sqlite"));

    let outcome = run_council(
        "which db?",
        members,
        chairman,
        None,
        &store,
        std::env::temp_dir(),
        Duration::from_secs(30),
    )
    .await
    .unwrap();

    assert_eq!(outcome.synthesis, "final: use sqlite");
    assert_eq!(outcome.answers.len(), 2);
    assert!(outcome.transcript["answers"].is_array());
    // Usage recorded for all stages: 2 answers + 2 reviews + 1 synthesis = 5 runs
    let (codex_in, _) = store.totals_since(Provider::Codex, 0).unwrap();
    assert!(codex_in > 0);
}

#[tokio::test]
async fn council_fails_when_all_members_fail() {
    let store = QuotaStore::open_in_memory().unwrap();
    let members = vec![CouncilMember {
        label: "w1".into(),
        adapter: Arc::new(ScriptedAdapter::failing(Provider::Codex, "quota exhausted")),
        model: None,
    }];
    let chairman = Arc::new(ScriptedAdapter::ok_with_text(Provider::Claude, "unused"));
    let err = run_council("q", members, chairman, None, &store, std::env::temp_dir(), Duration::from_secs(30))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no council member produced an answer"));
}

#[tokio::test]
async fn council_proceeds_when_one_member_fails() {
    let store = QuotaStore::open_in_memory().unwrap();
    let members = vec![
        CouncilMember {
            label: "ok".into(),
            adapter: Arc::new(ScriptedAdapter::ok_with_text(Provider::Gemini, "answer")),
            model: None,
        },
        CouncilMember {
            label: "broken".into(),
            adapter: Arc::new(ScriptedAdapter::failing(Provider::Codex, "boom")),
            model: None,
        },
    ];
    let chairman = Arc::new(ScriptedAdapter::ok_with_text(Provider::Claude, "synthesized"));
    let outcome = run_council("q", members, chairman, None, &store, std::env::temp_dir(), Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(outcome.answers.len(), 1);
    assert_eq!(outcome.failed_members, vec!["broken".to_string()]);
}
```

- [ ] **Step 3: RED**, then implement `core/src/orchestrator/council.rs`:

```rust
use super::prompts;
use super::runner::{run_to_completion, RunStatus};
use crate::adapters::{Adapter, RunRequest};
use crate::quota::QuotaStore;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub struct CouncilMember {
    pub label: String,
    pub adapter: Arc<dyn Adapter>,
    /// Model passed to the CLI (`--model`/`-m`); None = CLI default.
    pub model: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct Score {
    pub agent: String,
    pub score: u8,
    #[serde(default)]
    pub justification: String,
}

#[derive(Debug, Deserialize)]
struct ScoresEnvelope {
    scores: Vec<Score>,
}

/// Extracts a `{"scores":[...]}` JSON object from model output: first tries
/// fenced ```json blocks, then the first `{...}` containing `"scores"`.
/// Returns None on any parse failure — councils tolerate sloppy reviewers.
pub fn parse_scores(text: &str) -> Option<Vec<Score>> {
    let candidate = if let Some(start) = text.find("```json") {
        let rest = &text[start + 7..];
        let end = rest.find("```")?;
        rest[..end].trim().to_string()
    } else {
        let start = text.find('{')?;
        text[start..].trim().to_string()
    };
    serde_json::from_str::<ScoresEnvelope>(&candidate)
        .ok()
        .map(|e| e.scores)
}

pub struct CouncilOutcome {
    pub synthesis: String,
    /// (anonymized label, member label, answer text)
    pub answers: Vec<(String, String, String)>,
    pub failed_members: Vec<String>,
    pub transcript: serde_json::Value,
}

pub async fn run_council(
    question: &str,
    members: Vec<CouncilMember>,
    chairman: Arc<dyn Adapter>,
    chairman_model: Option<String>,
    quota: &QuotaStore,
    cwd: PathBuf,
    timeout: Duration,
) -> anyhow::Result<CouncilOutcome> {
    // Stage 1: independent answers, in parallel.
    let answer_prompt = prompts::council_answer(question);
    let futures: Vec<_> = members
        .iter()
        .map(|m| {
            let req = RunRequest {
                prompt: answer_prompt.clone(),
                model: m.model.clone(),
                cwd: cwd.clone(),
            };
            run_to_completion(m.adapter.clone(), req, quota, timeout)
        })
        .collect();
    let results = futures::future::join_all(futures).await;

    let mut answers: Vec<(String, String, String)> = Vec::new(); // (anon label, member label, text)
    let mut failed_members: Vec<String> = Vec::new();
    for (member, result) in members.iter().zip(results) {
        match result {
            Ok(outcome) if matches!(outcome.status, RunStatus::Completed) && !outcome.final_text.is_empty() => {
                answers.push((String::new(), member.label.clone(), outcome.final_text));
            }
            _ => failed_members.push(member.label.clone()),
        }
    }
    if answers.is_empty() {
        anyhow::bail!("no council member produced an answer");
    }

    // Anonymize: assign labels A, B, C... in shuffled order.
    use rand::seq::SliceRandom;
    let mut order: Vec<usize> = (0..answers.len()).collect();
    order.shuffle(&mut rand::thread_rng());
    for (anon_idx, original_idx) in order.iter().enumerate() {
        answers[*original_idx].0 = char::from(b'A' + anon_idx as u8).to_string();
    }
    answers.sort_by(|a, b| a.0.cmp(&b.0));

    // Stage 2: each surviving member reviews the anonymized set, in parallel.
    let anon_pairs: Vec<(&str, &str)> = answers
        .iter()
        .map(|(label, _, text)| (label.as_str(), text.as_str()))
        .collect();
    let review_prompt = prompts::council_review(question, &anon_pairs);
    let surviving: Vec<&CouncilMember> = members
        .iter()
        .filter(|m| !failed_members.contains(&m.label))
        .collect();
    let review_futures: Vec<_> = surviving
        .iter()
        .map(|m| {
            let req = RunRequest { prompt: review_prompt.clone(), model: m.model.clone(), cwd: cwd.clone() };
            run_to_completion(m.adapter.clone(), req, quota, timeout)
        })
        .collect();
    let review_results = futures::future::join_all(review_futures).await;

    let mut reviews: Vec<String> = Vec::new();
    let mut scores: Vec<(String, Option<Vec<Score>>)> = Vec::new();
    for (member, result) in surviving.iter().zip(review_results) {
        if let Ok(outcome) = result {
            if matches!(outcome.status, RunStatus::Completed) {
                scores.push((member.label.clone(), parse_scores(&outcome.final_text)));
                reviews.push(outcome.final_text);
            }
        }
    }

    // Stage 3: chairman synthesis.
    let review_refs: Vec<&str> = reviews.iter().map(String::as_str).collect();
    let synthesis_prompt = prompts::council_synthesis(question, &anon_pairs, &review_refs);
    let synthesis_outcome = run_to_completion(
        chairman,
        RunRequest { prompt: synthesis_prompt, model: chairman_model, cwd },
        quota,
        timeout,
    )
    .await?;
    if !matches!(synthesis_outcome.status, RunStatus::Completed) {
        anyhow::bail!("chairman failed to synthesize: {:?}", synthesis_outcome.status);
    }

    let transcript = serde_json::json!({
        "kind": "council",
        "question": question,
        "answers": answers.iter().map(|(anon, member, text)| serde_json::json!({
            "anon_label": anon, "member": member, "text": text
        })).collect::<Vec<_>>(),
        "failed_members": failed_members,
        "reviews": reviews,
        "scores": scores.iter().map(|(member, s)| serde_json::json!({
            "member": member,
            "parsed": s.as_ref().map(|v| v.iter().map(|sc| serde_json::json!({
                "agent": sc.agent, "score": sc.score, "justification": sc.justification
            })).collect::<Vec<_>>()),
        })).collect::<Vec<_>>(),
        "synthesis": synthesis_outcome.final_text,
    });

    Ok(CouncilOutcome {
        synthesis: synthesis_outcome.final_text,
        answers,
        failed_members,
        transcript,
    })
}
```

Add dependencies to `core/Cargo.toml` `[dependencies]`: `rand = "0.8"` and `futures = "0.3"`.

Design notes (preserve in code comments where marked):
- `CouncilMember.model` / `chairman_model` carry the per-role model into every RunRequest; CLI wiring (Task 8) fills them from config, tests pass `None`.
- Anonymization shuffle prevents positional bias; the anon↔member mapping is preserved in the transcript only.

- [ ] **Step 4: GREEN (54 = 48 + 3 unit + 3 integration), fmt+clippy, commit** `feat: council — answers, anonymized cross-review, chairman synthesis`

### Task 7: review — diff audit (TDD)

**Files:**
- Create: `core/src/orchestrator/review.rs` (uncomment in mod.rs)
- Create: `core/tests/review_test.rs`

- [ ] **Step 1: Failing unit tests for verdict parsing** (bottom of review.rs):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_findings_block() {
        let text = "Analysis...\n```json\n{\"findings\":[{\"severity\":\"critical\",\"file\":\"a.rs\",\"description\":\"sql injection\"}]}\n```";
        let v = parse_verdict(text).unwrap();
        assert_eq!(v.findings.len(), 1);
        assert!(matches!(v.findings[0].severity, Severity::Critical));
    }

    #[test]
    fn empty_findings_means_clean() {
        let v = parse_verdict("```json\n{\"findings\":[]}\n```").unwrap();
        assert!(v.findings.is_empty());
        assert!(!v.has_critical());
    }

    #[test]
    fn malformed_verdict_yields_none() {
        assert!(parse_verdict("looks good to me!").is_none());
    }

    #[test]
    fn unknown_severity_maps_to_minor() {
        let v = parse_verdict("```json\n{\"findings\":[{\"severity\":\"catastrophic\",\"file\":\"x\",\"description\":\"d\"}]}\n```").unwrap();
        assert!(matches!(v.findings[0].severity, Severity::Minor));
    }
}
```

- [ ] **Step 2: Failing integration test** `core/tests/review_test.rs`:

```rust
mod common;

use common::ScriptedAdapter;
use consilium::event::Provider;
use consilium::orchestrator::review::run_review;
use consilium::quota::QuotaStore;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn reviews_a_diff_and_returns_verdict() {
    let store = QuotaStore::open_in_memory().unwrap();
    let reviewer = Arc::new(ScriptedAdapter::ok_with_text(
        Provider::Codex,
        "```json\n{\"findings\":[{\"severity\":\"important\",\"file\":\"main.rs\",\"description\":\"unwrap on user input\"}]}\n```",
    ));
    let result = run_review(
        "--- a/main.rs\n+++ b/main.rs\n+let x = input.unwrap();",
        reviewer,
        None,
        &store,
        std::env::temp_dir(),
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    let verdict = result.verdict.expect("verdict parsed");
    assert_eq!(verdict.findings.len(), 1);
    assert!(!verdict.has_critical());
    assert!(result.transcript["raw_review"].is_string());
}

#[tokio::test]
async fn unparseable_review_still_returns_raw_text() {
    let store = QuotaStore::open_in_memory().unwrap();
    let reviewer = Arc::new(ScriptedAdapter::ok_with_text(Provider::Gemini, "LGTM, ship it"));
    let result = run_review("diff", reviewer, None, &store, std::env::temp_dir(), Duration::from_secs(30))
        .await
        .unwrap();
    assert!(result.verdict.is_none());
    assert_eq!(result.raw_review, "LGTM, ship it");
}
```

- [ ] **Step 3: RED**, then implement `core/src/orchestrator/review.rs`:

```rust
use super::prompts;
use super::runner::{run_to_completion, RunStatus};
use crate::adapters::{Adapter, RunRequest};
use crate::quota::QuotaStore;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Critical,
    Important,
    Minor,
}

// Unknown severities map to Minor: tolerate creative reviewers.
impl Default for Severity {
    fn default() -> Self {
        Severity::Minor
    }
}

#[derive(Debug, Deserialize)]
pub struct Finding {
    #[serde(deserialize_with = "lenient_severity")]
    pub severity: Severity,
    #[serde(default)]
    pub file: String,
    pub description: String,
}

fn lenient_severity<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Severity, D::Error> {
    let s = String::deserialize(d)?;
    Ok(match s.as_str() {
        "critical" => Severity::Critical,
        "important" => Severity::Important,
        _ => Severity::Minor,
    })
}

#[derive(Debug, Deserialize)]
pub struct Verdict {
    pub findings: Vec<Finding>,
}

impl Verdict {
    pub fn has_critical(&self) -> bool {
        self.findings.iter().any(|f| matches!(f.severity, Severity::Critical))
    }
}

/// Same lenient extraction strategy as council::parse_scores.
pub fn parse_verdict(text: &str) -> Option<Verdict> {
    let candidate = if let Some(start) = text.find("```json") {
        let rest = &text[start + 7..];
        let end = rest.find("```")?;
        rest[..end].trim().to_string()
    } else {
        let start = text.find('{')?;
        text[start..].trim().to_string()
    };
    serde_json::from_str::<Verdict>(&candidate).ok()
}

pub struct ReviewResult {
    pub verdict: Option<Verdict>,
    pub raw_review: String,
    pub transcript: serde_json::Value,
}

pub async fn run_review(
    diff: &str,
    reviewer: Arc<dyn Adapter>,
    reviewer_model: Option<String>,
    quota: &QuotaStore,
    cwd: PathBuf,
    timeout: Duration,
) -> anyhow::Result<ReviewResult> {
    let prompt = prompts::diff_review(diff);
    let outcome = run_to_completion(
        reviewer,
        RunRequest { prompt, model: reviewer_model, cwd },
        quota,
        timeout,
    )
    .await?;
    if !matches!(outcome.status, RunStatus::Completed) {
        anyhow::bail!("reviewer failed: {:?}", outcome.status);
    }
    let verdict = parse_verdict(&outcome.final_text);
    let transcript = serde_json::json!({
        "kind": "review",
        "diff_bytes": diff.len(),
        "raw_review": outcome.final_text,
        "parsed": verdict.is_some(),
    });
    Ok(ReviewResult { verdict, raw_review: outcome.final_text, transcript })
}
```

- [ ] **Step 4: GREEN (60), fmt+clippy, commit** `feat: review — diff audit with lenient verdict parsing`

### Task 8: CLI wiring — `council` and `review` subcommands

**Files:**
- Modify: `core/src/main.rs`

- [ ] **Step 1: Add subcommands to the clap enum:**

```rust
    /// Convene the council: independent answers, anonymized cross-review, synthesis
    Council {
        question: String,
        /// Hard per-session timeout in seconds
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
    /// Audit a git diff with the reviewer role
    Review {
        /// Review staged changes instead of unstaged
        #[arg(long)]
        staged: bool,
        /// Read the diff from a file instead of running git
        #[arg(long)]
        diff_file: Option<std::path::PathBuf>,
        #[arg(long, default_value_t = 600)]
        timeout: u64,
    },
```

- [ ] **Step 2: Implement the arms.** Council arm: load `Config` (from `consilium.config.json` in cwd if present, else defaults), build members from `config.roles.workers` (label = `"{provider}-{model}"`, adapter via `roles::adapter_for`, model passed through per the Task 6 required shape), chairman from `config.roles.chairman`. Run `run_council`, then:

```rust
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            let transcripts = consilium::orchestrator::transcript::TranscriptStore::new(
                consilium::orchestrator::transcript::TranscriptStore::default_base()?,
            );
            let outcome = consilium::orchestrator::council::run_council(
                &question, members, chairman_adapter, &store,
                std::env::current_dir()?, std::time::Duration::from_secs(timeout),
            ).await?;
            let path = transcripts.save("council", &outcome.transcript)?;
            println!("\n════ COUNCIL SYNTHESIS ════\n");
            println!("{}", outcome.synthesis);
            if !outcome.failed_members.is_empty() {
                println!("\n(members failed: {})", outcome.failed_members.join(", "));
            }
            println!("\ntranscript: {}", path.display());
```

Review arm (complete):

```rust
        Command::Review { staged, diff_file, timeout } => {
            use consilium::orchestrator::{review, roles, transcript::TranscriptStore};

            let cwd = std::env::current_dir()?;
            let diff = match diff_file {
                Some(path) => std::fs::read_to_string(&path)
                    .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?,
                None => {
                    let mut cmd = std::process::Command::new("git");
                    cmd.arg("diff").current_dir(&cwd);
                    if staged {
                        cmd.arg("--staged");
                    }
                    let out = cmd.output()?;
                    if !out.status.success() {
                        anyhow::bail!("git diff failed: {}", String::from_utf8_lossy(&out.stderr));
                    }
                    String::from_utf8_lossy(&out.stdout).into_owned()
                }
            };
            if diff.trim().is_empty() {
                anyhow::bail!("nothing to review: the diff is empty");
            }

            let config = consilium::config::Config::load(Some(std::path::Path::new("consilium.config.json")))?;
            let reviewer_role = &config.roles.reviewer;
            let reviewer = roles::adapter_for(reviewer_role);
            let store = consilium::quota::QuotaStore::open(&quota_db_path()?)?;
            let transcripts = TranscriptStore::new(TranscriptStore::default_base()?);

            let result = review::run_review(
                &diff,
                reviewer,
                Some(reviewer_role.model.clone()),
                &store,
                cwd,
                std::time::Duration::from_secs(timeout),
            )
            .await?;
            let path = transcripts.save("review", &result.transcript)?;

            match &result.verdict {
                Some(v) if v.findings.is_empty() => println!("✓ clean — no findings"),
                Some(v) => {
                    for f in &v.findings {
                        println!("[{:?}] {} — {}", f.severity, f.file, f.description);
                    }
                }
                None => {
                    println!("(reviewer output was not structured JSON — raw review below)\n");
                    println!("{}", result.raw_review);
                    println!("\ntranscript: {}", path.display());
                    // An unparseable security review must fail CLOSED: CI can
                    // distinguish "critical found" (2) from "review unusable" (3).
                    std::process::exit(3);
                }
            }
            println!("\ntranscript: {}", path.display());
            if result.verdict.as_ref().is_some_and(|v| v.has_critical()) {
                std::process::exit(2);
            }
        }
```

- [ ] **Step 3: Gates:** `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test` (60 green; CLI arms have no unit tests — they are wiring, covered by the E2E smoke next).

- [ ] **Step 4: Commit** `feat: council and review CLI commands`

### Task 9: E2E smoke on real providers (sanctioned quota spend)

**Files:** none (verification only) — possibly small adapter/prompt fixes if reality bites.

- [ ] **Step 1: Real council run** (spends ~2-5 requests across codex+gemini+claude — sanctioned once):

```bash
cd /tmp && mkdir -p consilium-smoke && cd consilium-smoke
source "$HOME/.cargo/env"
/Users/temur/Desktop/Claude/consilium/target/debug/consilium council \
  "In a Rust CLI, anyhow or thiserror for a small binary crate? One paragraph." \
  --timeout 300
```

Expected: synthesis paragraph printed; transcript path printed; `consilium quota` shows usage on all three providers. Inspect the transcript JSON: answers from both workers, reviews present, scores parsed (at least one member). If a provider misbehaves (flag drift, JSON non-compliance), fix the adapter/prompt, re-run unit tests, re-smoke.

- [ ] **Step 2: Real review run:**

```bash
cd /Users/temur/Desktop/Claude/consilium
git diff HEAD~1 HEAD > /tmp/review-smoke.diff
./target/debug/consilium review --diff-file /tmp/review-smoke.diff --timeout 300
```

Expected: findings (or clean verdict) printed; exit code 0 or 2 consistent with severity.

- [ ] **Step 3: Update README** — add `council` and `review` to Quick start with one-line descriptions; move M2 row in the status table: split into "M2a — council & review ✅" and "M2b — conduct, auto, supervisor 🚧 next".

- [ ] **Step 4: Final gates + commit** `feat: M2a deliberation complete — council and review verified on real providers`

---

## M2a Exit Criteria

- `cargo test` green (≥60 tests), clippy `-D warnings` clean, fmt clean.
- `consilium council "<question>"` completes a real 3-stage deliberation across the configured providers, prints a synthesis, writes a transcript, records quota.
- `consilium review --diff-file <f>` returns a structured verdict; exit code 2 on critical findings, exit code 3 when the reviewer output is unparseable (fail closed).
- Zero quota spent by the test suite (scripted adapters only); real spend confined to the two smoke runs.

## Next plan (after M2a ships)

**M2b:** `conduct` (decompose → dispatch → accept/rework loop, stateless re-prompting), supervisor (between-step gate + watchdog on event streams), `auto` pipeline, quota-aware worker routing, review arbiter on author/reviewer disputes.
