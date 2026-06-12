use super::{Adapter, RunRequest};
use crate::event::{AgentEvent, Provider};
use tokio::process::Command;

pub struct GeminiAdapter;

/// Best-effort extraction of token usage from gemini's stats blob, summed
/// ACROSS ALL models — a single run may use several (e.g. a utility router
/// plus the main model). Usage is optional — absence is not an error.
///
/// Real recorded stats shape (gemini CLI --output-format json):
///   stats.models.<model>.tokens.{ input, prompt, candidates, total, cached, thoughts, tool }
/// `prompt` and `input` are the same value; we read `prompt` (matches plan field name).
fn extract_usage(stats: &serde_json::Value) -> Option<AgentEvent> {
    let models = stats.get("models")?.as_object()?;
    let mut input = 0u64;
    let mut output = 0u64;
    let mut found = false;
    for model in models.values() {
        let Some(tokens) = model.get("tokens") else {
            continue;
        };
        found = true;
        // M1 counts all input-side tokens together; M2 quota-$ conversion will split by cache rate.
        input += tokens["prompt"].as_u64().unwrap_or(0) + tokens["cached"].as_u64().unwrap_or(0);
        output +=
            tokens["candidates"].as_u64().unwrap_or(0) + tokens["thoughts"].as_u64().unwrap_or(0);
        // 'total' is derived; 'tool' tokens excluded pending semantics check (TODO M2).
    }
    found.then_some(AgentEvent::Usage {
        input_tokens: input,
        output_tokens: output,
    })
}

impl Adapter for GeminiAdapter {
    fn provider(&self) -> Provider {
        Provider::Gemini
    }

    fn cli_binary(&self) -> &'static str {
        "gemini"
    }

    fn build_command(&self, req: &RunRequest) -> Command {
        let mut cmd = Command::new(self.cli_binary());
        cmd.arg("-p")
            .arg(&req.prompt)
            .arg("--output-format")
            .arg("json");
        if let Some(model) = &req.model {
            cmd.arg("-m").arg(model);
        }
        if req.write {
            cmd.arg("--approval-mode").arg("auto_edit");
        }
        cmd.current_dir(&req.cwd);
        cmd
    }

    fn parse_final(&self, full_output: &str) -> Vec<AgentEvent> {
        let trimmed = full_output.trim();
        if trimmed.is_empty() {
            return vec![AgentEvent::Failed {
                error: "gemini produced no output".into(),
            }];
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            let Some(response) = v["response"].as_str() else {
                // Valid JSON without "response" is an error envelope (quota,
                // auth, ...) — surface it as Failed, never as fake success.
                return vec![AgentEvent::Failed {
                    error: format!("gemini JSON response had no 'response' field: {trimmed}"),
                }];
            };
            let mut events = Vec::new();
            if let Some(sid) = v["session_id"].as_str() {
                events.push(AgentEvent::SessionStarted {
                    session_id: sid.to_string(),
                    provider: Provider::Gemini,
                    model: None, // model name is per-stats entry, not top-level
                });
            }
            events.push(AgentEvent::Message {
                text: response.to_string(),
            });
            if let Some(usage) = v.get("stats").and_then(extract_usage) {
                events.push(usage);
            }
            events.push(AgentEvent::Completed {
                result: Some(response.to_string()),
            });
            return events;
        }
        // Falls here only on JSON parse error — genuine plain-text output from older CLIs.
        vec![
            AgentEvent::Message {
                text: trimmed.to_string(),
            },
            AgentEvent::Completed {
                result: Some(trimmed.to_string()),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AgentEvent;

    const FIXTURE: &str = include_str!("../../tests/fixtures/gemini/basic_response.json");

    #[test]
    fn parses_json_response_fixture() {
        let events = GeminiAdapter.parse_final(FIXTURE);
        assert!(matches!(
            events.first(),
            Some(AgentEvent::SessionStarted { session_id, .. }) if session_id == "test-session-1"
        ));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Message { text } if text == "ok")));
        // Two models in the fixture: (12+0) + (100+10) = 122 in, (3+0) + (20+5) = 28 out.
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Usage {
                input_tokens: 122,
                output_tokens: 28
            }
        )));
        assert!(
            matches!(events.last().unwrap(), AgentEvent::Completed { result: Some(r) } if r == "ok")
        );
    }

    #[test]
    fn plain_text_output_falls_back_to_message() {
        let events = GeminiAdapter.parse_final("just plain text\n");
        assert!(matches!(&events[0], AgentEvent::Message { text } if text == "just plain text"));
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::Completed { .. }
        ));
    }

    #[test]
    fn empty_output_yields_failed() {
        let events = GeminiAdapter.parse_final("   \n");
        assert!(matches!(&events[0], AgentEvent::Failed { .. }));
    }

    #[test]
    fn json_error_envelope_maps_to_failed() {
        let events = GeminiAdapter.parse_final(r#"{"error":"quota exceeded"}"#);
        assert!(matches!(
            &events[0],
            AgentEvent::Failed { error } if error.contains("no 'response' field")
        ));
    }

    fn command_args(advisory: bool, write: bool) -> Vec<String> {
        let req = RunRequest {
            prompt: "hi".into(),
            model: Some("gemini-3-pro".into()),
            cwd: std::env::temp_dir(),
            advisory,
            write,
        };
        GeminiAdapter
            .build_command(&req)
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn build_command_uses_json_output() {
        let args = command_args(false, false);
        assert!(args.contains(&"-p".to_string()));
        assert!(args.windows(2).any(|w| w == ["--output-format", "json"]));
        assert!(args.windows(2).any(|w| w == ["-m", "gemini-3-pro"]));
    }

    #[test]
    fn write_run_enables_scoped_edits() {
        let args = command_args(false, true);
        assert!(args
            .windows(2)
            .any(|w| w == ["--approval-mode", "auto_edit"]));
    }

    #[test]
    fn deliberation_run_has_no_write_flags() {
        let args = command_args(false, false);
        assert!(!args.contains(&"--approval-mode".to_string()));
    }

    /// Runs against the real recorded fixture (exists since Task 4).
    #[test]
    fn parses_recorded_real_output_if_present() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/gemini/recorded/basic.json"
        );
        let Ok(raw) = std::fs::read_to_string(path) else {
            eprintln!("skipped: no recorded fixture");
            return;
        };
        if raw.trim().is_empty() {
            eprintln!("skipped: empty recorded fixture");
            return;
        }
        let events = GeminiAdapter.parse_final(&raw);
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionStarted { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Message { .. })));
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::Completed { .. }
        ));
    }

    /// Locks parser-vs-reality: the recorded fixture has TWO models
    /// (gemini-3.1-flash-lite: prompt=1189/candidates=44/thoughts=91 and
    /// gemini-3-flash-preview: prompt=7359/candidates=1/thoughts=61, cached=0
    /// in both), so usage must SUM across them, not take the first only.
    #[test]
    fn usage_sums_across_models_in_recorded_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/gemini/recorded/basic.json"
        );
        let Ok(raw) = std::fs::read_to_string(path) else {
            eprintln!("skipped: no recorded fixture");
            return;
        };
        if raw.trim().is_empty() {
            eprintln!("skipped: empty recorded fixture");
            return;
        }
        let events = GeminiAdapter.parse_final(&raw);
        // input = 1189 + 7359 = 8548; output = (44 + 91) + (1 + 61) = 197.
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Usage {
                input_tokens: 8548,
                output_tokens: 197
            }
        )));
    }
}
