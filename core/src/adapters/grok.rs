use super::{asked_to_sign_in, Adapter, FailureKind, RunRequest};
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
/// [--permission-mode acceptEdits]`, which emits one JSON object per line.
/// What a real run looks like (grok 1.0.92, recorded 2026-11-22 —
/// `tests/fixtures/grok/recorded/basic.jsonl`): the answer streams as
/// `{"type":"text","data":"…"}` deltas and reasoning as `"thought"` deltas;
/// a tool call is `{"type":"tool_call","toolName":…,"rawInput":…}`; and the
/// turn closes with `{"type":"end","stopReason":…,"usage":{…}}`, which holds
/// the run's total usage but no text. So `parse_line` reports tool calls,
/// usage and errors as they arrive, and `parse_final` joins the deltas into
/// whole messages and completes the run with the answer.
///
/// The schema is BETA per xAI's own docs, so the older shapes this adapter
/// was first written against are still understood (a `text`/`content` field,
/// a `tool` name, `stopReason` and the answer on one line — the documented
/// `--output-format json` envelope), and anything unrecognised is ignored
/// rather than treated as an error. The fixtures directly under
/// `tests/fixtures/grok/` are synthetic and cover those older shapes.
///
/// Permission gating (verified against grok 0.2.87 and 1.0.92): write (worker) runs get
/// `--permission-mode acceptEdits` — auto-approve file edits only, the same
/// mode name claude uses — never `--always-approve` (approve-everything).
/// Advisory (read-only deliberation) runs pass no permission flag at all and
/// rely on the headless default plus the prompt contract, mirroring how
/// `gemini.rs`/`agy` gates `--dangerously-skip-permissions` on `write` only.
/// The CLI also exposes `--sandbox <PROFILE>` — worth adopting for advisory
/// runs once its profile values are documented.
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
    let name = tool_name(obj)?;
    let detail = obj
        .get("rawInput")
        .or_else(|| obj.get("input"))
        .or_else(|| obj.get("args"))
        .or_else(|| obj.get("arguments"))
        .cloned()
        .unwrap_or_else(|| whole.clone());
    Some(AgentEvent::ToolCall {
        name: name.to_string(),
        detail: detail.to_string(),
    })
}

/// The tool a tool-shaped object names — see [`tool_call`].
fn tool_name(obj: &Map<String, Value>) -> Option<&str> {
    obj.get("tool")
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
        .filter(|s| !s.is_empty())
}

/// The fragment of text a streamed delta carries (grok 1.0.92):
/// `{"type":"text","data":"…"}` for the answer, `"thought"` for reasoning.
fn delta(obj: &Map<String, Value>) -> Option<(&str, &str)> {
    let kind = obj.get("type")?.as_str()?;
    let data = obj.get("data")?.as_str()?;
    matches!(kind, "text" | "thought").then_some((kind, data))
}

/// What grok said over one streamed turn, put back together from its deltas.
#[derive(Debug, Default, PartialEq)]
struct StreamedTurn {
    /// What it said before each tool call, in order.
    narration: Vec<String>,
    /// What it said after its last tool call: the answer.
    answer: Option<String>,
}

impl StreamedTurn {
    fn read(lines: &[Map<String, Value>]) -> Self {
        let mut narration = Vec::new();
        let mut current = String::new();
        for obj in lines {
            if let Some(("text", data)) = delta(obj) {
                current.push_str(data);
            } else if tool_name(obj).is_some() && !current.is_empty() {
                narration.push(std::mem::take(&mut current));
            }
        }
        Self {
            narration,
            answer: Some(current).filter(|text| !text.is_empty()),
        }
    }

    fn said_anything(&self) -> bool {
        self.answer.is_some() || !self.narration.is_empty()
    }

    /// Each message whole, in the order grok said them.
    fn into_messages(self) -> impl Iterator<Item = AgentEvent> {
        self.narration
            .into_iter()
            .chain(self.answer)
            .map(|text| AgentEvent::Message { text })
    }

    /// Each message whole, then the run's completion with the answer.
    fn into_events(self) -> Vec<AgentEvent> {
        let completed = AgentEvent::Completed {
            result: self.answer.clone(),
        };
        self.into_messages()
            .chain(std::iter::once(completed))
            .collect()
    }
}

/// How grok's error opens when it has no saved sign-in (grok 1.0.92, recorded
/// 2026-11-22 — `tests/fixtures/grok/recorded/error_signed_out.jsonl`).
/// Lower-cased, as `classify_failure` compares it.
const NOT_SIGNED_IN: &str = "not signed in.";

/// What older grok printed instead, before waiting — with no time limit — for
/// a device sign-in (grok 0.2.87, recorded 2026-11-22). Kept so that an
/// outdated CLI is stopped rather than waited on.
const SIGN_IN_PROMPT: &str = "To sign in, open this URL in your browser";

impl GrokAdapter {
    fn asked_to_sign_in(&self, error: &str) -> bool {
        asked_to_sign_in(self.cli_binary(), self.sign_in_prompts(), error)
    }
}

impl Adapter for GrokAdapter {
    fn provider(&self) -> Provider {
        Provider::Grok
    }

    fn cli_binary(&self) -> &'static str {
        "grok"
    }

    fn sign_in_prompts(&self) -> &'static [&'static str] {
        &[SIGN_IN_PROMPT]
    }

    fn build_command(&self, req: &RunRequest) -> Command {
        // Flags verified against the real CLI (grok 0.2.87, 2026-07-08; 1.0.92, 2026-11-22):
        // `-p/--single`, `--output-format streaming-json`, `-m/--model`,
        // `--permission-mode <default|acceptEdits|auto|dontAsk|bypassPermissions|plan>`.
        // NOTE: `--no-auto-update` (assumed from pre-release research) does NOT
        // exist in 0.2.87 — passing it fails argument parsing on every run.
        let mut cmd = Command::new(self.cli_binary());
        cmd.arg("-p")
            .arg(&req.prompt)
            .arg("--output-format")
            .arg("streaming-json");
        if let Some(model) = req.explicit_model() {
            cmd.arg("--model").arg(model);
        }
        // Worker (write:true) auto-approves file edits only — least privilege,
        // same mode name claude uses. Advisory runs pass no permission flag at
        // all (headless default denies mutating tools; see module doc).
        if req.write {
            cmd.arg("--permission-mode").arg("acceptEdits");
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

        // Real observed event (grok 0.2.87, recorded 2026-07-08): an error is
        // `{"type":"error","message":"..."}` — e.g. the 402 subscription-
        // required refusal. Map to Failed so the orchestrator sees the actual
        // message instead of only a bare non-zero exit + stderr tail.
        if obj.get("type").and_then(Value::as_str) == Some("error") {
            if let Some(message) = obj
                .get("message")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                return vec![AgentEvent::Failed {
                    error: message.to_string(),
                }];
            }
        }

        if is_terminal_marker(obj) {
            // The line that ends a turn carries its total usage. It carries the
            // answer too in the `--output-format json` envelope, but not when
            // streaming (grok 1.0.92): there the answer came as deltas, and
            // `parse_final` — which sees them all — completes the run.
            let answer = text_field(obj).map(|text| AgentEvent::Completed { result: Some(text) });
            return usage_event(obj).into_iter().chain(answer).collect();
        }

        // A streamed delta (grok 1.0.92: `{"type":"text","data":"…"}`, and
        // `thought` for reasoning) is a fragment of a message, not a message.
        if delta(obj).is_some() {
            return Vec::new();
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
        let lines: Vec<Map<String, Value>> = full_output
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
            .filter_map(|value| value.as_object().cloned())
            .collect();
        // `parse_line` already completed the run if the line that ended the
        // turn carried the answer; a second Completed would double it up.
        if lines
            .iter()
            .any(|obj| is_terminal_marker(obj) && text_field(obj).is_some())
        {
            return Vec::new();
        }
        let streamed = StreamedTurn::read(&lines);
        if lines.iter().any(is_terminal_marker) {
            return streamed.into_events();
        }
        // Deltas with no end after them: the stream was cut off. What grok
        // had said is reported; the run is not completed.
        if streamed.said_anything() {
            return streamed.into_messages().collect();
        }
        // Neither deltas nor a line that ended the turn: the CLI dropped to
        // some other shape. Salvage the last object that carries text.
        lines
            .iter()
            .rev()
            .find_map(text_field)
            .map(|text| vec![AgentEvent::Completed { result: Some(text) }])
            .unwrap_or_default()
    }

    fn classify_failure(&self, error: &str) -> FailureKind {
        let e = error.to_ascii_lowercase();
        // Unambiguous "this model doesn't exist" phrasing only — anything
        // fuzzier (context-length complaints, generic 4xx) must NOT trip this,
        // mirroring codex::classify_failure's context-length carve-out.
        if e.starts_with(NOT_SIGNED_IN) || self.asked_to_sign_in(error) {
            FailureKind::Unauthenticated
        } else if e.contains("model not found")
            || e.contains("unknown model")
            || e.contains("deprecated")
            || e.contains("does not exist")
        {
            FailureKind::ModelUnavailable
        } else {
            // Everything else is retried — including other auth-shaped text
            // ("401", "expired", "subscription required"): no real grok output
            // for a rejected sign-in has been recorded yet, and guessing wrong
            // would skip the whole provider. `consilium auth` reads those.
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
            model: Some("grok-build".into()),
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
        // `--no-auto-update` must NOT be passed: it does not exist in the
        // real CLI (verified against grok 0.2.87) and fails arg parsing.
        assert!(!args.contains(&"--no-auto-update".to_string()));
        assert!(args.windows(2).any(|w| w == ["--model", "grok-build"]));
        // Advisory: no permission flag at all.
        assert!(!args.contains(&"--permission-mode".to_string()));
    }

    #[test]
    fn write_run_auto_approves_edits_only() {
        let args = command_args(false, true);
        assert!(args
            .windows(2)
            .any(|w| w == ["--permission-mode", "acceptEdits"]));
        // Never the approve-everything hammer.
        assert!(!args.contains(&"--always-approve".to_string()));
    }

    #[test]
    fn advisory_run_never_passes_permission_flag() {
        // advisory:true, write:false — the gate is on `write` alone.
        let args = command_args(true, false);
        assert!(!args.contains(&"--permission-mode".to_string()));
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

    /// Guesses at how grok words a rejected sign-in — none recorded from the
    /// real CLI, so none may skip the provider.
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

    /// Pinned on REAL recorded output (grok 0.2.87): the 402 subscription-
    /// required refusal streams as `{"type":"error","message":"..."}` and must
    /// surface as Failed with the message, and classify as auth-shaped
    /// (Transient — prompts re-login/upgrade), never ModelUnavailable.
    #[test]
    fn recorded_subscription_error_maps_to_failed_and_transient() {
        let raw = include_str!("../../tests/fixtures/grok/recorded/error_subscription.jsonl");
        let events = parse_all(raw);
        let Some(AgentEvent::Failed { error }) = events.first() else {
            panic!("expected Failed from recorded error event, got {events:?}");
        };
        assert!(error.contains("402"), "got: {error}");
        assert_eq!(
            GrokAdapter.classify_failure(error),
            FailureKind::Transient,
            "subscription refusal must not kill the model in ModelHealth"
        );
    }

    /// Real grok 1.0.92 output for a turn that reads a file, then answers
    /// (recorded 2026-11-22; paths, ids and signatures replaced, the list of
    /// the machine's own commands cut). Not the shape this adapter was first
    /// written against: the answer arrives as `text` deltas and the closing
    /// `end` event carries none.
    const RECORDED: &str = include_str!("../../tests/fixtures/grok/recorded/basic.jsonl");

    const RECORDED_ANSWER: &str = "The file hello.txt contains a single line that reads \"The launch code is PINEAPPLE-42.\" That is the entire contents of the file.";

    /// Everything a session reports for `raw`: line by line, then at the end.
    fn run_events(raw: &str) -> Vec<AgentEvent> {
        let mut events = parse_all(raw);
        events.extend(GrokAdapter.parse_final(raw));
        events
    }

    #[test]
    fn a_recorded_run_completes_once_with_the_streamed_answer() {
        let completed: Vec<Option<String>> = run_events(RECORDED)
            .into_iter()
            .filter_map(|event| match event {
                AgentEvent::Completed { result } => Some(result),
                _ => None,
            })
            .collect();
        assert_eq!(completed, [Some(RECORDED_ANSWER.to_string())]);
    }

    /// One message per stretch of text — what grok said before its tool call,
    /// then the answer — never one per token, and never its reasoning.
    #[test]
    fn a_recorded_run_reports_each_message_whole() {
        let messages: Vec<String> = run_events(RECORDED)
            .into_iter()
            .filter_map(|event| match event {
                AgentEvent::Message { text } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(
            messages,
            [
                "I'll read `hello.txt` and then summarize it in two sentences.",
                RECORDED_ANSWER
            ]
        );
    }

    /// grok reports usage after each model call and again, summed, when the
    /// turn ends. Counting both would double the bill.
    #[test]
    fn a_recorded_run_counts_its_tokens_once() {
        let usage: Vec<(u64, u64)> = run_events(RECORDED)
            .into_iter()
            .filter_map(|event| match event {
                AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                } => Some((input_tokens, output_tokens)),
                _ => None,
            })
            .collect();
        assert_eq!(usage, [(44250, 323)]);
    }

    #[test]
    fn a_recorded_run_reports_its_tool_call_with_the_input_alone() {
        let calls: Vec<(String, String)> = run_events(RECORDED)
            .into_iter()
            .filter_map(|event| match event {
                AgentEvent::ToolCall { name, detail } => Some((name, detail)),
                _ => None,
            })
            .collect();
        assert_eq!(
            calls,
            [(
                "read_file".to_string(),
                r#"{"target_file":"/work/hello.txt"}"#.to_string()
            )]
        );
    }

    /// A turn that ends without having said anything is still over.
    #[test]
    fn a_streamed_turn_with_no_text_completes_without_a_result() {
        let raw = r#"{"type":"end","stopReason":"end_turn","sessionId":"session-1"}"#;
        assert!(matches!(
            run_events(raw).as_slice(),
            [AgentEvent::Completed { result: None }]
        ));
    }

    fn first_failure(raw: &str) -> String {
        raw.lines()
            .flat_map(|l| GrokAdapter.parse_line(l))
            .find_map(|e| match e {
                AgentEvent::Failed { error } => Some(error),
                _ => None,
            })
            .expect("recorded stream carries a Failed event")
    }

    /// Real grok 0.2.87 output for a retired model id (`grok-build`, recorded
    /// 2026-11-17): the model is gone, so the ladder must mark it dead.
    #[test]
    fn recorded_unknown_model_is_model_unavailable() {
        let raw = include_str!("../../tests/fixtures/grok/recorded/error_unknown_model.jsonl");
        let error = first_failure(raw);
        assert!(error.contains("unknown model id"), "got: {error}");
        assert_eq!(
            GrokAdapter.classify_failure(&error),
            FailureKind::ModelUnavailable
        );
    }

    /// Real grok 0.2.87 output once xAI started refusing old clients (HTTP 426,
    /// recorded 2026-11-17). The CLI is too old — the MODEL is fine — so this
    /// must not mark the model dead.
    #[test]
    fn recorded_outdated_cli_is_not_model_unavailable() {
        let raw = include_str!("../../tests/fixtures/grok/recorded/error_outdated_cli.jsonl");
        let error = first_failure(raw);
        assert!(error.contains("outdated"), "got: {error}");
        assert_eq!(GrokAdapter.classify_failure(&error), FailureKind::Transient);
    }

    /// Real grok 0.2.87 stderr with no saved sign-in (recorded 2026-11-22
    /// against an empty home folder; the one-time code is blanked). grok
    /// starts a device sign-in and waits for it with no time limit.
    const SIGNED_OUT_STDERR: &str = "\nTo sign in, open this URL in your browser:\n\n  https://accounts.x.ai/oauth2/device?user_code=XXXX-XXXX\n\nConfirm this code in your browser:\n\n  XXXX-XXXX\n\nOnly continue with a code you requested. Don't share it with anyone.\n\nWaiting for authorization...\n";

    #[test]
    fn groks_sign_in_prompt_is_what_a_signed_out_grok_prints() {
        assert!(GrokAdapter
            .sign_in_prompts()
            .iter()
            .any(|prompt| SIGNED_OUT_STDERR.contains(prompt)));
    }

    /// grok 0.2.87 never gave up by itself; this is what a run stopped from
    /// outside (a timeout) fails with.
    #[test]
    fn classifies_a_signed_out_grok_0_2_87_as_unauthenticated() {
        let error = format!(
            "process exited with signal: 15 (SIGTERM); stderr tail: {}",
            SIGNED_OUT_STDERR.trim()
        );
        assert_eq!(
            GrokAdapter.classify_failure(&error),
            FailureKind::Unauthenticated
        );
    }

    /// Real grok 1.0.92 output with no saved sign-in (recorded 2026-11-22
    /// against an empty home folder): it no longer waits — it fails at once
    /// and says so.
    #[test]
    fn recorded_signed_out_grok_is_unauthenticated() {
        let raw = include_str!("../../tests/fixtures/grok/recorded/error_signed_out.jsonl");
        let error = first_failure(raw);
        assert!(error.starts_with("Not signed in."), "got: {error}");
        assert_eq!(
            GrokAdapter.classify_failure(&error),
            FailureKind::Unauthenticated
        );
        assert!(crate::auth::is_auth_failure(&error));
    }

    /// grok's own words further on in stderr are something a working grok
    /// logged — a worker editing this very file, say — not a sign-out.
    #[test]
    fn groks_prompt_quoted_later_in_stderr_is_no_sign_out() {
        let error = format!(
            "process exited with exit status: 1; stderr tail: edited grok.rs{SIGNED_OUT_STDERR}"
        );
        assert_eq!(GrokAdapter.classify_failure(&error), FailureKind::Transient);
    }

    /// Deltas with no `end` after them: the stream was cut off. What grok had
    /// said so far is reported, but a run that died mid-answer is never a
    /// completed one.
    #[test]
    fn a_stream_cut_off_before_its_end_is_not_a_completed_run() {
        let raw =
            "{\"type\":\"text\",\"data\":\"The answer\"}\n{\"type\":\"text\",\"data\":\" is\"}\n";
        let events = run_events(raw);
        assert!(
            matches!(events.as_slice(), [AgentEvent::Message { text }] if text == "The answer is"),
            "got: {events:?}"
        );
    }
}
