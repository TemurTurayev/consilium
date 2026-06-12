use super::{Adapter, RunRequest};
use crate::event::{AgentEvent, Provider};
use tokio::process::Command;

pub struct CodexAdapter;

impl Adapter for CodexAdapter {
    fn provider(&self) -> Provider {
        Provider::Codex
    }

    fn cli_binary(&self) -> &'static str {
        "codex"
    }

    fn build_command(&self, req: &RunRequest) -> Command {
        let mut cmd = Command::new(self.cli_binary());
        cmd.arg("exec").arg("--json");
        // codex refuses to run outside a trusted/git directory. Advisory runs
        // (council/review — read-only deliberation) may legitimately run
        // anywhere, so they opt out; execution/write runs keep the safeguard.
        if req.advisory {
            cmd.arg("--skip-git-repo-check");
        }
        if let Some(model) = &req.model {
            cmd.arg("-m").arg(model);
        }
        cmd.arg(&req.prompt);
        cmd.current_dir(&req.cwd);
        cmd
    }

    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            return Vec::new();
        };
        match v["type"].as_str() {
            Some("thread.started") => vec![AgentEvent::SessionStarted {
                session_id: v["thread_id"].as_str().unwrap_or_default().to_string(),
                provider: Provider::Codex,
                // Real thread.started (codex-cli 0.139) carries no model field — stays None.
                model: None,
            }],
            Some("item.completed") => {
                let item = &v["item"];
                match item["type"].as_str() {
                    Some("agent_message") => vec![AgentEvent::Message {
                        text: item["text"].as_str().unwrap_or_default().to_string(),
                    }],
                    Some("reasoning") => vec![AgentEvent::Thinking {
                        text: item["text"].as_str().unwrap_or_default().to_string(),
                    }],
                    // TODO(M2): real codex may emit file-change items — inspect Task 9 recording and map them to AgentEvent::FileChanged instead of the catch-all.
                    Some(other) => vec![AgentEvent::ToolCall {
                        name: other.to_string(),
                        detail: item.to_string(),
                    }],
                    None => Vec::new(),
                }
            }
            Some("turn.completed") => {
                let mut events = Vec::new();
                if let Some(u) = v.get("usage") {
                    // OpenAI semantics: cached_input_tokens is a SUBSET of input_tokens — do
                    // NOT sum (unlike Claude, where cache tokens are disjoint additions).
                    // M2 quota-$ conversion will use the cached share for discount math.
                    events.push(AgentEvent::Usage {
                        input_tokens: u["input_tokens"].as_u64().unwrap_or(0),
                        output_tokens: u["output_tokens"].as_u64().unwrap_or(0)
                            + u["reasoning_output_tokens"].as_u64().unwrap_or(0),
                    });
                }
                events.push(AgentEvent::Completed { result: None });
                events
            }
            Some("turn.failed") | Some("error") => vec![AgentEvent::Failed {
                error: v["error"]["message"]
                    .as_str()
                    .or(v["message"].as_str())
                    .unwrap_or("unknown error")
                    .to_string(),
            }],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{AgentEvent, Provider};

    const FIXTURE: &str = include_str!("../../tests/fixtures/codex/basic_session.jsonl");

    fn parse_all(raw: &str) -> Vec<AgentEvent> {
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .flat_map(|l| CodexAdapter.parse_line(l))
            .collect()
    }

    #[test]
    fn parses_basic_session_fixture() {
        let events = parse_all(FIXTURE);
        assert!(
            matches!(&events[0], AgentEvent::SessionStarted { provider: Provider::Codex, session_id, .. } if session_id == "th_1")
        );
        assert!(events.iter().any(
            |e| matches!(e, AgentEvent::ToolCall { name, .. } if name == "command_execution")
        ));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Message { text } if text == "ok")));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Usage {
                input_tokens: 40,
                output_tokens: 8
            }
        )));
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::Completed { .. }
        ));
    }

    #[test]
    fn turn_failed_maps_to_failed() {
        let line = r#"{"type":"turn.failed","error":{"message":"usage limit reached"}}"#;
        let events = CodexAdapter.parse_line(line);
        assert!(
            matches!(&events[0], AgentEvent::Failed { error } if error == "usage limit reached")
        );
    }

    #[test]
    fn garbage_line_yields_no_events() {
        assert!(CodexAdapter.parse_line("???").is_empty());
        assert!(CodexAdapter
            .parse_line(r#"{"type":"unknown.kind"}"#)
            .is_empty());
    }

    fn command_args(advisory: bool) -> Vec<String> {
        let req = RunRequest {
            prompt: "hi".into(),
            model: Some("gpt-5.4".into()),
            cwd: std::env::temp_dir(),
            advisory,
        };
        CodexAdapter
            .build_command(&req)
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn build_command_uses_exec_json() {
        let args = command_args(false);
        assert_eq!(args[0], "exec");
        assert!(args.contains(&"--json".to_string()));
        assert!(args.windows(2).any(|w| w == ["-m", "gpt-5.4"]));
        assert_eq!(args.last().unwrap(), "hi");
    }

    #[test]
    fn advisory_run_skips_git_repo_check() {
        let args = command_args(true);
        assert!(args.contains(&"--skip-git-repo-check".to_string()));
    }

    #[test]
    fn execution_run_keeps_git_repo_safeguard() {
        let args = command_args(false);
        assert!(!args.contains(&"--skip-git-repo-check".to_string()));
    }

    #[test]
    fn usage_does_not_double_count_cached_subset() {
        let line = r#"{"type":"turn.completed","usage":{"input_tokens":140,"cached_input_tokens":100,"output_tokens":6,"reasoning_output_tokens":4}}"#;
        let events = CodexAdapter.parse_line(line);
        assert!(matches!(
            &events[0],
            AgentEvent::Usage {
                input_tokens: 140,
                output_tokens: 10
            }
        ));
    }

    /// Runs only when real fixtures have been recorded via script/record_fixtures.sh.
    #[test]
    fn parses_recorded_real_output_if_present() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/codex/recorded/basic.jsonl"
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
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Usage {
                input_tokens: 10324,
                output_tokens: 5
            }
        )));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { .. })));
    }
}
