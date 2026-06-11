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
        Self {
            provider,
            script,
            delay_secs: 0,
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
    fn build_command(&self, _req: &RunRequest) -> tokio::process::Command {
        debug_assert!(
            !self.script.lines().any(|l| l == "CONSILIUM_EOF"),
            "ScriptedAdapter: script contains the literal heredoc delimiter 'CONSILIUM_EOF' as a standalone line; output will be truncated"
        );
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
