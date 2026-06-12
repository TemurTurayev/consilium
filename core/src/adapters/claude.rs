use super::{Adapter, RunRequest};
use crate::event::{AgentEvent, Provider};
use tokio::process::Command;

pub struct ClaudeAdapter;

impl Adapter for ClaudeAdapter {
    fn provider(&self) -> Provider {
        Provider::Claude
    }

    fn cli_binary(&self) -> &'static str {
        "claude"
    }

    fn build_command(&self, req: &RunRequest) -> Command {
        let mut cmd = Command::new(self.cli_binary());
        // --verbose is required for stream-json to emit the system/init line
        cmd.arg("-p")
            .arg(&req.prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose");
        if let Some(model) = &req.model {
            cmd.arg("--model").arg(model);
        }
        // `advisory` has no per-flag effect for this CLI (no codex-style
        // trusted-dir refusal to opt out of); the field is read by sessions::spawn's
        // invariant check only.
        if req.write {
            cmd.arg("--permission-mode").arg("acceptEdits");
        }
        cmd.current_dir(&req.cwd);
        cmd
    }

    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            return Vec::new();
        };
        match v["type"].as_str() {
            Some("system") if v["subtype"] == "init" => vec![AgentEvent::SessionStarted {
                session_id: v["session_id"].as_str().unwrap_or_default().to_string(),
                provider: Provider::Claude,
                model: v["model"].as_str().map(String::from),
            }],
            Some("assistant") => {
                let mut events = Vec::new();
                if let Some(blocks) = v["message"]["content"].as_array() {
                    for block in blocks {
                        match block["type"].as_str() {
                            Some("text") => events.push(AgentEvent::Message {
                                text: block["text"].as_str().unwrap_or_default().to_string(),
                            }),
                            Some("thinking") => events.push(AgentEvent::Thinking {
                                text: block["thinking"].as_str().unwrap_or_default().to_string(),
                            }),
                            Some("tool_use") => events.push(AgentEvent::ToolCall {
                                name: block["name"].as_str().unwrap_or_default().to_string(),
                                detail: block["input"].to_string(),
                            }),
                            _ => {}
                        }
                    }
                }
                events
            }
            Some("result") => {
                let mut events = Vec::new();
                if let Some(u) = v.get("usage") {
                    // M1 counts all input-side tokens together; M2 quota-$ conversion will split by cache rate.
                    let input = u["input_tokens"].as_u64().unwrap_or(0)
                        + u["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                        + u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                    events.push(AgentEvent::Usage {
                        input_tokens: input,
                        output_tokens: u["output_tokens"].as_u64().unwrap_or(0),
                    });
                }
                if v["is_error"].as_bool().unwrap_or(false) {
                    events.push(AgentEvent::Failed {
                        error: v["result"].as_str().unwrap_or("unknown error").to_string(),
                    });
                } else {
                    events.push(AgentEvent::Completed {
                        result: v["result"].as_str().map(String::from),
                    });
                }
                events
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{AgentEvent, Provider};

    const FIXTURE: &str = include_str!("../../tests/fixtures/claude/basic_session.jsonl");

    fn parse_all(raw: &str) -> Vec<AgentEvent> {
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .flat_map(|l| ClaudeAdapter.parse_line(l))
            .collect()
    }

    #[test]
    fn parses_basic_session_fixture() {
        let events = parse_all(FIXTURE);
        assert!(
            matches!(&events[0], AgentEvent::SessionStarted { provider: Provider::Claude, session_id, .. } if session_id == "abc123")
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Message { text } if text == "ok")));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Usage {
                input_tokens: 42,
                output_tokens: 5
            }
        )));
        assert!(
            matches!(events.last().unwrap(), AgentEvent::Completed { result: Some(r) } if r == "ok")
        );
    }

    #[test]
    fn error_result_maps_to_failed() {
        let line = r#"{"type":"result","subtype":"error","is_error":true,"result":"limit reached","session_id":"abc123"}"#;
        let events = ClaudeAdapter.parse_line(line);
        assert!(matches!(&events[0], AgentEvent::Failed { error } if error == "limit reached"));
    }

    #[test]
    fn garbage_line_yields_no_events() {
        assert!(ClaudeAdapter.parse_line("not json at all").is_empty());
        assert!(ClaudeAdapter
            .parse_line(r#"{"type":"unknown_kind"}"#)
            .is_empty());
    }

    fn command_args(advisory: bool, write: bool) -> Vec<String> {
        let req = RunRequest {
            prompt: "hi".into(),
            model: Some("sonnet".into()),
            cwd: std::env::temp_dir(),
            advisory,
            write,
        };
        ClaudeAdapter
            .build_command(&req)
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn build_command_uses_stream_json_and_model() {
        let args = command_args(false, false);
        assert!(args
            .windows(2)
            .any(|w| w == ["--output-format", "stream-json"]));
        assert!(args.contains(&"--model".to_string()));
        assert!(args.contains(&"sonnet".to_string()));
    }

    #[test]
    fn write_run_enables_scoped_edits() {
        let args = command_args(false, true);
        assert!(args
            .windows(2)
            .any(|w| w == ["--permission-mode", "acceptEdits"]));
    }

    #[test]
    fn deliberation_run_has_no_write_flags() {
        let args = command_args(false, false);
        assert!(!args.contains(&"--permission-mode".to_string()));
    }

    #[test]
    fn usage_sums_cache_tokens() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"abc123","usage":{"input_tokens":6,"cache_creation_input_tokens":100,"cache_read_input_tokens":10,"output_tokens":6}}"#;
        let events = ClaudeAdapter.parse_line(line);
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Usage {
                input_tokens: 116,
                output_tokens: 6
            }
        )));
    }

    /// Runs only when real fixtures have been recorded via script/record_fixtures.sh.
    #[test]
    fn parses_recorded_real_output_if_present() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/claude/recorded/basic.jsonl"
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
        assert!(matches!(
            events.first(),
            Some(AgentEvent::SessionStarted { .. })
        ));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Message { .. })));
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Usage { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { .. })));
    }
}
