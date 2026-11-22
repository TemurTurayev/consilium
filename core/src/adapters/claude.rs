use super::{Adapter, FailureKind, RunRequest};
use crate::event::{AgentEvent, Provider};
use tokio::process::Command;

pub struct ClaudeAdapter;

/// The built-in tools a deliberation run may use: read-only ones.
const DELIBERATION_TOOLS: &str = "Read,Grep,Glob";

impl Adapter for ClaudeAdapter {
    fn provider(&self) -> Provider {
        Provider::Claude
    }

    fn cli_binary(&self) -> &'static str {
        "claude"
    }

    /// The long-lived token `claude setup-token` prints — what a headless
    /// `claude -p` signs in with when it has no interactive login of its own.
    fn credential_env(&self) -> &'static [&'static str] {
        &["CLAUDE_CODE_OAUTH_TOKEN"]
    }

    fn build_command(&self, req: &RunRequest) -> Command {
        let mut cmd = Command::new(self.cli_binary());
        // --verbose is required for stream-json to emit the system/init line
        cmd.arg("-p")
            .arg(&req.prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose");
        if let Some(model) = req.explicit_model() {
            cmd.arg("--model").arg(model);
        }
        // Never the user's own MCP servers: a nested run would start all of
        // them again (a chat bot, a browser, Consilium's own server), pay for
        // their tool lists on every call, and hand an unattended agent the
        // user's connectors.
        cmd.arg("--strict-mcp-config");
        if req.write {
            cmd.arg("--permission-mode").arg("acceptEdits");
        }
        // Deliberation answers a question and may read the code it is about —
        // nothing more. `--safe-mode` drops plugins, hooks, skills and
        // CLAUDE.md, which only distract here (a SessionStart hook can redirect
        // the model outright). A write run keeps them: it edits the user's
        // project and should follow that project's instructions.
        // `--tools` takes a list, so it goes last — nothing may follow it that
        // could be read as one more tool name.
        // Flags verified against Claude Code 2.1.292 (2026-11-22).
        if req.advisory {
            cmd.arg("--safe-mode")
                .arg("--tools")
                .arg(DELIBERATION_TOOLS);
        }
        cmd.current_dir(&req.cwd);
        cmd
    }

    fn classify_failure(&self, error: &str) -> FailureKind {
        let e = error.to_ascii_lowercase();
        if e.contains("may not exist or you may not have access")
            || e.contains("issue with the selected model")
        {
            FailureKind::ModelUnavailable
        } else if e.contains("please run /login")
            || (e.contains("failed to authenticate") && e.contains("api error"))
        {
            // The CLI's own wording only (see the tests for the recorded
            // strings): this verdict skips every Claude rung for the run, so
            // an error that merely talks about logging in must not earn it.
            FailureKind::Unauthenticated
        } else if e.contains("rate limit") || e.contains("usage limit") {
            FailureKind::RateLimited
        } else {
            FailureKind::Transient
        }
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
    use crate::adapters::FailureKind;
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

    fn has_flag(args: &[String], flag: &str) -> bool {
        args.iter().any(|a| a == flag)
    }

    /// A nested `claude -p` used to start every MCP server the user has — a
    /// Telegram bot, a browser, Consilium's own server again — and paid for
    /// their tool lists on each call (measured 2026-11-22 on Claude Code
    /// 2.1.292: 51k input tokens for "Reply with: ok", 18k without them).
    #[test]
    fn no_run_starts_the_users_mcp_servers() {
        for (advisory, write) in [(false, false), (true, false), (false, true)] {
            assert!(
                has_flag(&command_args(advisory, write), "--strict-mcp-config"),
                "advisory={advisory} write={write}"
            );
        }
    }

    /// Deliberation is a question and an answer. Plugins, hooks and skills
    /// only get in the way — a SessionStart hook can tell the model to do
    /// something else entirely — and nothing but reading is needed (4k input
    /// tokens instead of 18k, same measurement).
    #[test]
    fn a_deliberation_run_is_bare_and_can_only_read() {
        let args = command_args(true, false);
        assert!(has_flag(&args, "--safe-mode"));
        assert!(args.windows(2).any(|w| w == ["--tools", "Read,Grep,Glob"]));
    }

    /// `--tools` takes a list: anything after it that is not a flag would be
    /// read as one more tool name. It must come last, far from the prompt.
    #[test]
    fn the_tool_list_cannot_swallow_another_argument() {
        let args = command_args(true, false);
        assert_eq!(&args[..2], ["-p", "hi"]);
        assert_eq!(args.last().map(String::as_str), Some("Read,Grep,Glob"));
    }

    /// A worker edits the user's project, so the project's own instructions
    /// and settings still apply to it, and it keeps its full tool set.
    #[test]
    fn a_write_run_keeps_project_customizations_and_all_tools() {
        let args = command_args(false, true);
        assert!(!has_flag(&args, "--safe-mode"));
        assert!(!has_flag(&args, "--tools"));
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

    #[test]
    fn classifies_model_unavailable() {
        let e = "There's an issue with the selected model (claude-fable-5). It may not exist or you may not have access to it.";
        assert_eq!(
            ClaudeAdapter.classify_failure(e),
            FailureKind::ModelUnavailable
        );
    }

    /// Both strings are what Claude Code 2.1.292 really prints for a headless
    /// run with no usable sign-in (recorded 2026-11-22): the first with a stale
    /// host session in the environment, the second with a clean one.
    #[test]
    fn classifies_a_missing_sign_in_as_unauthenticated() {
        for error in [
            "Failed to authenticate. API Error: 401 OAuth access token is invalid.",
            "Not logged in · Please run /login",
        ] {
            assert_eq!(
                ClaudeAdapter.classify_failure(error),
                FailureKind::Unauthenticated,
                "{error}"
            );
        }
    }

    /// Being signed out takes every Claude rung out of the run, so only the
    /// CLI's own wording may say so — not an error that merely talks about
    /// logging in (a failing test, a hook's stderr, the task itself).
    #[test]
    fn an_error_that_only_mentions_logging_in_is_not_a_sign_out() {
        for error in [
            "test failed: expected the banner 'You are not logged in'",
            "hook error: failed to authenticate with the package registry",
            "process exited with exit status: 1; stderr tail: login required for npm publish",
        ] {
            assert_eq!(
                ClaudeAdapter.classify_failure(error),
                FailureKind::Transient,
                "{error}"
            );
        }
    }

    #[test]
    fn classifies_rate_limit() {
        assert_eq!(
            ClaudeAdapter.classify_failure("Claude usage limit reached; try again later"),
            FailureKind::RateLimited
        );
    }

    #[test]
    fn classifies_other_as_transient() {
        assert_eq!(
            ClaudeAdapter.classify_failure("connection reset by peer"),
            FailureKind::Transient
        );
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
