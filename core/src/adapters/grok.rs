use super::{Adapter, FailureKind, RunRequest};
use crate::event::{AgentEvent, Provider};
use serde_json::{Map, Value};
use tokio::process::Command;

/// The "Grok" provider is reached through xAI's **Grok Build CLI** (`grok`),
/// authenticated via browser OAuth tied to an X Premium+/SuperGrok
/// subscription (first interactive run opens the browser; the token is then
/// cached — `XAI_API_KEY` is the metered API-key fallback, not the path this
/// adapter takes).
///
/// Headless usage: `grok -p "<prompt>" --output-format streaming-json
/// --no-auto-update [--always-approve]`, which emits one JSON object per
/// line (NDJSON). **The exact event schema is BETA per xAI's own docs and may
/// churn** — `parse_line`/`parse_final` below are deliberately defensive:
/// they key off plausible field shapes (`text`/`content`, a tool name, a
/// `stopReason`) rather than a fixed `type` tag, and silently ignore
/// anything they don't recognize instead of erroring. Recorded real output
/// (`script/record_fixtures.sh`) should replace these assumptions once the
/// CLI is actually installed somewhere — see the synthetic fixtures under
/// `tests/fixtures/grok/` for exactly what shape is currently assumed.
///
/// Unlike codex (`--skip-git-repo-check`) or claude (no such gap at all),
/// Grok Build has **no documented read-only/sandbox flag** — the only
/// approval-related flag xAI documents is `--always-approve`, which we only
/// pass for write (worker) runs. Advisory (read-only deliberation) runs pass
/// no approval flag at all and rely on the prompt contract (the caller never
/// asks Grok to edit files in an advisory run) — this mirrors how
/// `gemini.rs`/`agy` handles the same "CLI has no dedicated read-only mode"
/// gap: `--dangerously-skip-permissions` there is *also* gated on `write`
/// only, not on `advisory`.
pub struct GrokAdapter;

/// True when `obj` looks like a terminal/turn-ending event: a non-empty
/// `stopReason` string, or a `done`/`final` boolean flag. Schema-defensive —
/// xAI's beta docs only confirm `stopReason` (in the non-streaming `--output-
/// format json` envelope `{text, stopReason, sessionId, requestId}`); the
/// `done`/`final` fallbacks are a hedge in case the streaming NDJSON shape
/// signals completion differently.
fn is_terminal_marker(obj: &Map<String, Value>) -> bool {
    let has_stop_reason = obj
        .get("stopReason")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    let has_final_flag = obj.get("done").and_then(Value::as_bool).unwrap_or(false)
        || obj.get("final").and_then(Value::as_bool).unwrap_or(false);
    has_stop_reason || has_final_flag
}

/// Extract a `text`/`content` string field, if present and non-empty.
fn text_field(obj: &Map<String, Value>) -> Option<String> {
    obj.get("text")
        .or_else(|| obj.get("content"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Only emits `Usage` when the JSON clearly carries BOTH an input and an
/// output token count (nested under `usage`, camelCase or snake_case) — a
/// single ambiguous number is not enough. Absence (the common case while the
/// schema is beta) is intentional: the engine's estimate-on-no-usage
/// fallback (`orchestrator::runner::run_to_completion`) records a flagged
/// estimate automatically, exactly like gemini/agy.
fn usage_event(obj: &Map<String, Value>) -> Option<AgentEvent> {
    let usage = obj.get("usage")?.as_object()?;
    let input = usage
        .get("inputTokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)?;
    let output = usage
        .get("outputTokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)?;
    Some(AgentEvent::Usage {
        input_tokens: input,
        output_tokens: output,
    })
}

/// Tool-shaped object → `(name, detail)`. Looks for an explicit `tool`/
/// `toolName` string field, or a `type` field naming a tool-call variant
/// alongside a `name` field — the same two conventions claude (`tool_use`)
/// and codex (`item.type` on `item.completed`) use, generalized since Grok's
/// exact tag is undocumented.
fn tool_call(obj: &Map<String, Value>, whole: &Value) -> Option<AgentEvent> {
    let name = obj
        .get("tool")
        .or_else(|| obj.get("toolName"))
        .and_then(Value::as_str)
        .or_else(|| {
            let ty = obj.get("type")?.as_str()?;
            if matches!(ty, "tool_call" | "tool_use" | "toolCall" | "toolUse") {
                obj.get("name").and_then(Value::as_str)
            } else {
                None
            }
        })
        .filter(|s| !s.is_empty())?;
    let detail = obj
        .get("input")
        .or_else(|| obj.get("args"))
        .or_else(|| obj.get("arguments"))
        .cloned()
        .unwrap_or_else(|| whole.clone());
    Some(AgentEvent::ToolCall {
        name: name.to_string(),
        detail: detail.to_string(),
    })
}

impl Adapter for GrokAdapter {
    fn provider(&self) -> Provider {
        Provider::Grok
    }

    fn cli_binary(&self) -> &'static str {
        "grok"
    }

    fn build_command(&self, req: &RunRequest) -> Command {
        let mut cmd = Command::new(self.cli_binary());
        cmd.arg("-p")
            .arg(&req.prompt)
            .arg("--output-format")
            .arg("streaming-json")
            .arg("--no-auto-update");
        if let Some(model) = &req.model {
            cmd.arg("--model").arg(model);
        }
        // Worker (write:true) runs auto-approve so Grok applies file edits
        // unattended, same shape as gemini/agy's `--dangerously-skip-permissions`
        // gate. Advisory runs pass no approval flag at all (see module doc).
        if req.write {
            cmd.arg("--always-approve");
        }
        cmd.current_dir(&req.cwd);
        cmd
    }

    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            return Vec::new();
        };
        let Some(obj) = value.as_object() else {
            return Vec::new();
        };

        if is_terminal_marker(obj) {
            let mut events = Vec::new();
            if let Some(usage) = usage_event(obj) {
                events.push(usage);
            }
            events.push(AgentEvent::Completed {
                result: text_field(obj),
            });
            return events;
        }

        if let Some(ev) = tool_call(obj, &value) {
            return vec![ev];
        }

        if let Some(text) = text_field(obj) {
            return vec![AgentEvent::Message { text }];
        }

        Vec::new()
    }

    fn parse_final(&self, full_output: &str) -> Vec<AgentEvent> {
        // parse_line already emits Completed for any line carrying a terminal
        // marker; re-scanning here would double it up. Only fall back to
        // salvaging a result when NO line in the whole stream looked terminal
        // (e.g. the CLI dropped to non-streaming `--output-format json`, or the
        // beta NDJSON shape drifted away from `stopReason`/`done`/`final`).
        let saw_terminal = full_output.lines().any(|line| {
            serde_json::from_str::<Value>(line.trim())
                .ok()
                .and_then(|v| v.as_object().map(is_terminal_marker))
                .unwrap_or(false)
        });
        if saw_terminal {
            return Vec::new();
        }

        full_output
            .lines()
            .rev()
            .find_map(|line| {
                let value: Value = serde_json::from_str(line.trim()).ok()?;
                let obj = value.as_object()?;
                text_field(obj)
            })
            .map(|text| vec![AgentEvent::Completed { result: Some(text) }])
            .unwrap_or_default()
    }

    fn classify_failure(&self, error: &str) -> FailureKind {
        let e = error.to_ascii_lowercase();
        // Unambiguous "this model doesn't exist" phrasing only — anything
        // fuzzier (context-length complaints, generic 4xx) must NOT trip this,
        // mirroring codex::classify_failure's context-length carve-out.
        if e.contains("model not found")
            || e.contains("unknown model")
            || e.contains("deprecated")
            || e.contains("does not exist")
        {
            FailureKind::ModelUnavailable
        } else if e.contains("unauthorized")
            || e.contains("401")
            || e.contains("403")
            || e.contains("login")
            || e.contains("expired")
            || e.contains("subscription required")
        {
            // Auth-shaped: the cached browser-OAuth token is missing/stale.
            // None of claude/codex/gemini's classify_failure carve out a
            // dedicated auth FailureKind either (there isn't one — see
            // adapters::mod::FailureKind) — every adapter's auth-shaped
            // strings fall into the generic Transient bucket here, because
            // re-login clears the condition on the next run rather than
            // marking the model permanently dead. The user-facing "needs
            // login" signal is a separate pipeline (`crate::auth::is_auth_failure`,
            // `consilium auth`/`consilium doctor`), not this one.
            FailureKind::Transient
        } else {
            FailureKind::Transient
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::FailureKind;
    use crate::event::AgentEvent;

    const BASIC: &str = include_str!("../../tests/fixtures/grok/basic_session.jsonl");
    const GARBAGE: &str = include_str!("../../tests/fixtures/grok/garbage_lines.jsonl");
    const ERROR_CASE: &str = include_str!("../../tests/fixtures/grok/error_case.jsonl");

    fn parse_all(raw: &str) -> Vec<AgentEvent> {
        raw.lines()
            .flat_map(|l| GrokAdapter.parse_line(l))
            .collect()
    }

    fn command_args(advisory: bool, write: bool) -> Vec<String> {
        let req = RunRequest {
            prompt: "hi".into(),
            model: Some("grok-build-0.1".into()),
            cwd: std::env::temp_dir(),
            advisory,
            write,
        };
        GrokAdapter
            .build_command(&req)
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn build_command_uses_streaming_json_and_model() {
        let args = command_args(false, false);
        assert!(args.contains(&"-p".to_string()));
        assert!(args
            .windows(2)
            .any(|w| w == ["--output-format", "streaming-json"]));
        assert!(args.contains(&"--no-auto-update".to_string()));
        assert!(args.windows(2).any(|w| w == ["--model", "grok-build-0.1"]));
        // Advisory: no approval flag at all — no read-only sandbox flag exists.
        assert!(!args.contains(&"--always-approve".to_string()));
    }

    #[test]
    fn write_run_auto_approves_edits() {
        let args = command_args(false, true);
        assert!(args.contains(&"--always-approve".to_string()));
    }

    #[test]
    fn advisory_run_never_passes_approve_flag() {
        // advisory:true, write:false — mirrors gemini's advisory path: the gate
        // is on `write` alone, since the CLI has no dedicated read-only flag.
        let args = command_args(true, false);
        assert!(!args.contains(&"--always-approve".to_string()));
    }

    #[test]
    fn parses_basic_session_fixture() {
        let events = parse_all(BASIC);
        assert!(events.iter().any(
            |e| matches!(e, AgentEvent::Message { text } if text == "Looking at the repository structure now.")
        ));
        assert!(events.iter().any(
            |e| matches!(e, AgentEvent::ToolCall { name, detail } if name == "read_file" && detail.contains("src/lib.rs"))
        ));
        assert!(events.iter().any(
            |e| matches!(e, AgentEvent::Message { text } if text == "Found the function. Applying the fix.")
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Usage {
                input_tokens: 48,
                output_tokens: 9
            }
        )));
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::Completed { result: Some(r) } if r == "Done — added the hello() function."
        ));
    }

    #[test]
    fn garbage_lines_yield_no_events() {
        for line in GARBAGE.lines() {
            assert!(
                GrokAdapter.parse_line(line).is_empty(),
                "line unexpectedly produced events: {line:?}"
            );
        }
    }

    #[test]
    fn error_output_produces_no_fabricated_success_events() {
        // A human-readable (non-JSON) error line, as if the CLI crashed before
        // emitting any structured output — neither parse_line nor parse_final
        // may invent a Message/Completed out of it. The real Failed signal for
        // this case comes from sessions.rs's process-exit + stderr path, not
        // from adapter parsing.
        let events = parse_all(ERROR_CASE);
        assert!(events.is_empty(), "events: {events:?}");
        assert!(GrokAdapter.parse_final(ERROR_CASE).is_empty());
    }

    #[test]
    fn no_usage_field_emits_no_usage_event() {
        let line = r#"{"stopReason":"stop","text":"done","sessionId":"s1","requestId":"r1"}"#;
        let events = GrokAdapter.parse_line(line);
        assert!(!events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::Completed { result: Some(r) } if r == "done"
        ));
    }

    #[test]
    fn ambiguous_single_number_does_not_emit_usage() {
        // A lone "tokens" count is not clearly (input, output) — must not guess.
        let line = r#"{"stopReason":"stop","text":"done","tokens":42}"#;
        let events = GrokAdapter.parse_line(line);
        assert!(!events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));
    }

    #[test]
    fn parse_final_salvages_last_text_when_no_terminal_marker_seen() {
        // Simulates a stream where the CLI never emitted a stopReason/done/final
        // marker (e.g. it dropped to the non-streaming --output-format json
        // envelope). The last JSON object carrying "text" is salvaged as the
        // result, per spec.
        let raw = "{\"text\":\"partial one\"}\n{\"text\":\"the real answer\",\"sessionId\":\"s1\",\"requestId\":\"r1\"}\n";
        let events = GrokAdapter.parse_final(raw);
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::Completed { result: Some(r) } if r == "the real answer"
        ));
    }

    #[test]
    fn parse_final_is_a_noop_when_parse_line_already_saw_a_terminal_marker() {
        // The basic fixture's last line already carries stopReason, so
        // parse_final must not add a second Completed for the same stream.
        assert!(GrokAdapter.parse_final(BASIC).is_empty());
    }

    #[test]
    fn classifies_model_unavailable() {
        assert_eq!(
            GrokAdapter.classify_failure("error: unknown model 'grok-bogus'"),
            FailureKind::ModelUnavailable
        );
        assert_eq!(
            GrokAdapter.classify_failure("model not found"),
            FailureKind::ModelUnavailable
        );
        assert_eq!(
            GrokAdapter.classify_failure("this model is deprecated"),
            FailureKind::ModelUnavailable
        );
    }

    #[test]
    fn classifies_auth_shaped_strings_as_transient() {
        for msg in [
            "401 unauthorized",
            "403 forbidden",
            "please run `grok` again to login",
            "your session has expired",
            "subscription required",
        ] {
            assert_eq!(
                GrokAdapter.classify_failure(msg),
                FailureKind::Transient,
                "expected transient for {msg:?}"
            );
        }
    }

    #[test]
    fn context_length_error_is_transient_not_model_unavailable() {
        let msg = "This model's maximum context length is 256000 tokens. You requested 300000.";
        assert_eq!(GrokAdapter.classify_failure(msg), FailureKind::Transient);
    }

    #[test]
    fn unknown_error_is_transient() {
        assert_eq!(
            GrokAdapter.classify_failure("connection reset by peer"),
            FailureKind::Transient
        );
    }

    /// Runs only when real fixtures have been recorded via script/record_fixtures.sh.
    #[test]
    fn parses_recorded_real_output_if_present() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/grok/recorded/basic.jsonl"
        );
        let Ok(raw) = std::fs::read_to_string(path) else {
            eprintln!("skipped: no recorded fixture");
            return;
        };
        if raw.trim().is_empty() {
            eprintln!("skipped: empty recorded fixture");
            return;
        }
        let events = parse_all(&raw);
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Message { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { .. })));
    }
}
