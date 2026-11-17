use super::{Adapter, FailureKind, RunRequest};
use crate::event::{AgentEvent, Provider};
use tokio::process::Command;

/// The "Gemini" provider is reached through the **Antigravity CLI** (`agy`), not
/// the retired standalone `gemini` CLI (Google deprecated free-tier Gemini Code
/// Assist for it). `agy` is a multi-model gateway driven headlessly with
/// `agy -p <prompt> --model "<model>"`, where `<model>` is an Antigravity model
/// id (e.g. `gemini-4-argon-high`) or its display name ("Gemini 4 Argon
/// (High)") — run `agy models` to list both. Verified against agy 1.5.10.
///
/// `agy -p` prints a plain-text response by default, so there is no per-run
/// token accounting — the engine records a flagged estimate instead. (agy 1.5
/// also offers `--output-format json` with a real `usage` block; adopting it is
/// a tracked follow-up.) Worker (write) runs pass
/// `--dangerously-skip-permissions` so the agent applies file edits unattended.
pub struct GeminiAdapter;

impl Adapter for GeminiAdapter {
    fn provider(&self) -> Provider {
        Provider::Gemini
    }

    fn cli_binary(&self) -> &'static str {
        "agy"
    }

    fn classify_failure(&self, error: &str) -> FailureKind {
        let e = error.to_ascii_lowercase();
        // "invalid model selection … is not recognized as a known model" is what
        // agy 1.5 prints for a retired/unknown `--model` (see the recorded
        // fixture); the older phrasings are kept for earlier CLI versions.
        if e.contains("invalid model selection")
            || e.contains("not recognized as a known model")
            || e.contains("404")
            || e.contains("unknown model")
            || e.contains("not found")
        {
            FailureKind::ModelUnavailable
        } else if e.contains("resource_exhausted")
            || e.contains("429")
            || e.contains("quota")
            || e.contains("rate limit")
        {
            FailureKind::RateLimited
        } else {
            FailureKind::Transient
        }
    }

    fn build_command(&self, req: &RunRequest) -> Command {
        let mut cmd = Command::new(self.cli_binary());
        // `agy -p` runs a single prompt non-interactively and prints the answer.
        cmd.arg("-p").arg(&req.prompt);
        if let Some(model) = req.explicit_model() {
            cmd.arg("--model").arg(model);
        }
        // Worker (write:true): auto-approve the agent's file edits so it runs
        // unattended. Advisory roles (write:false) ask only for a verdict and
        // need no tool permissions. (`advisory` itself has no per-flag effect;
        // it's enforced by sessions::spawn's invariant check.)
        if req.write {
            cmd.arg("--dangerously-skip-permissions");
        }
        cmd.current_dir(&req.cwd);
        cmd
    }

    fn parse_final(&self, full_output: &str) -> Vec<AgentEvent> {
        let trimmed = full_output.trim();
        if trimmed.is_empty() {
            return vec![AgentEvent::Failed {
                error: "agy (antigravity) produced no output".into(),
            }];
        }
        // `agy -p` emits a plain-text response (possibly with markdown). Consilium
        // captures the actual file changes separately via git diff, so the text is
        // just the worker's report / the advisory verdict.
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
    use crate::adapters::FailureKind;
    use crate::event::AgentEvent;

    fn command_args(advisory: bool, write: bool) -> Vec<String> {
        let req = RunRequest {
            prompt: "hi".into(),
            model: Some("Gemini 3.1 Pro (High)".into()),
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
    fn build_command_uses_agy_print_and_model() {
        let args = command_args(false, false);
        assert!(args.contains(&"-p".to_string()));
        assert!(args
            .windows(2)
            .any(|w| w == ["--model", "Gemini 3.1 Pro (High)"]));
        // Advisory: no write/permission flags, and no stale gemini-CLI flags.
        assert!(!args.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(!args.contains(&"--output-format".to_string()));
    }

    #[test]
    fn write_run_auto_approves_edits() {
        let args = command_args(false, true);
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn plain_text_output_becomes_message_and_completed() {
        let events = GeminiAdapter.parse_final("the answer\n");
        assert!(matches!(&events[0], AgentEvent::Message { text } if text == "the answer"));
        assert!(matches!(
            events.last().unwrap(),
            AgentEvent::Completed { result: Some(r) } if r == "the answer"
        ));
    }

    #[test]
    fn empty_output_yields_failed() {
        let events = GeminiAdapter.parse_final("   \n");
        assert!(matches!(&events[0], AgentEvent::Failed { .. }));
    }

    #[test]
    fn classifies_model_unavailable() {
        assert_eq!(
            GeminiAdapter.classify_failure("error: unknown model 'x'"),
            FailureKind::ModelUnavailable
        );
    }

    /// Real agy 1.5 stderr for a model name it no longer knows (recorded
    /// 2026-11-17, when "Gemini 3.1 Pro (High)" had been retired). It must mark
    /// the model dead so the ladder moves on instead of retrying a dead name.
    #[test]
    fn classifies_recorded_unknown_model_error_as_unavailable() {
        let stderr = include_str!("../../tests/fixtures/gemini/recorded/unknown_model.stderr.txt");
        let error = format!("process exited with exit status: 1; stderr tail: {stderr}");
        assert_eq!(
            GeminiAdapter.classify_failure(&error),
            FailureKind::ModelUnavailable
        );
    }

    #[test]
    fn classifies_rate_limit() {
        assert_eq!(
            GeminiAdapter.classify_failure("Error: RESOURCE_EXHAUSTED (429)"),
            FailureKind::RateLimited
        );
    }

    #[test]
    fn unknown_error_is_transient() {
        assert_eq!(
            GeminiAdapter.classify_failure("some unexpected failure"),
            FailureKind::Transient
        );
    }
}
