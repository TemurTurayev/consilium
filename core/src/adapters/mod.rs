pub mod claude;
pub mod codex;
pub mod gemini;
pub mod grok;

use crate::event::{AgentEvent, Provider};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub cwd: PathBuf,
    /// Advisory (read-only deliberation) run — council/review answering and
    /// reviewing, never mutating files. Adapters may relax workspace-trust
    /// safeguards that exist to protect against unwanted mutations (codex:
    /// `--skip-git-repo-check`). Execution/write runs MUST keep this false so
    /// provider safeguards stay armed (M2b conduct relies on that default).
    pub advisory: bool,
    /// Write-enabled execution run (conduct workers): the adapter passes its
    /// CLI's auto-approve-edits flag (re-verified 2026-11-17 against claude
    /// 2.1.292, codex-cli 0.160.1, agy 1.5.10; grok 0.2.87 by `--help` only):
    /// claude and grok `--permission-mode acceptEdits`, codex
    /// `--sandbox workspace-write`, agy `--dangerously-skip-permissions`.
    /// Deliberation runs keep this false — council/review must never mutate files.
    ///
    /// INVARIANT: `advisory` and `write` must not both be true (enforced by a
    /// hard `bail!` in sessions::spawn — real in release builds too). Design
    /// note: two orthogonal bools
    /// rather than a RunMode enum — they govern independent provider behaviors;
    /// an enum would need three variants to preserve the advisory-only case.
    pub write: bool,
}

/// Model name meaning "let the provider CLI pick its own default model". No
/// model flag is passed, so the choice can never go stale when a provider
/// renames or retires a model, and it always fits the account's subscription
/// tier. Valid anywhere a model is configured (`"model": "default"`).
pub const CLI_DEFAULT_MODEL: &str = "default";

impl RunRequest {
    /// The model to name on the provider CLI's command line, or `None` to let
    /// the CLI choose — when no model was requested or it is
    /// [`CLI_DEFAULT_MODEL`]. Every adapter reads the model through this.
    pub fn explicit_model(&self) -> Option<&str> {
        self.model.as_deref().filter(|m| *m != CLI_DEFAULT_MODEL)
    }
}

/// Why a session failed, for failover routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Model does not exist / no access (e.g. pulled like Fable). Permanent —
    /// the model is marked dead for the rest of the run.
    ModelUnavailable,
    /// Provider quota / rate limit hit. Temporary — demote, don't mark dead.
    RateLimited,
    /// Anything else (network, transient). Retry once, then demote.
    Transient,
}

/// An adapter knows how to launch one provider's CLI and translate its raw
/// output into AgentEvents. Parsing is PURE (no I/O) so it is fixture-testable.
pub trait Adapter: Send + Sync {
    fn provider(&self) -> Provider;
    fn cli_binary(&self) -> &'static str;
    fn build_command(&self, req: &RunRequest) -> tokio::process::Command;
    /// Streaming providers: one stdout line → zero or more events.
    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        let _ = line;
        Vec::new()
    }
    /// Non-streaming providers: full stdout at process exit → events.
    fn parse_final(&self, full_output: &str) -> Vec<AgentEvent> {
        let _ = full_output;
        Vec::new()
    }
    /// Classifies a failure message (from AgentEvent::Failed) for failover.
    /// Default: Transient. Each adapter overrides with patterns matched against
    /// its CLI's REAL error strings (see resilience tests).
    fn classify_failure(&self, error: &str) -> FailureKind {
        let _ = error;
        FailureKind::Transient
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn all_adapters() -> Vec<Arc<dyn Adapter>> {
        vec![
            Arc::new(claude::ClaudeAdapter),
            Arc::new(codex::CodexAdapter),
            Arc::new(gemini::GeminiAdapter),
            Arc::new(grok::GrokAdapter),
        ]
    }

    fn request(model: Option<&str>) -> RunRequest {
        RunRequest {
            prompt: "hi".into(),
            model: model.map(String::from),
            cwd: std::env::temp_dir(),
            advisory: false,
            write: false,
        }
    }

    fn args_for(adapter: &dyn Adapter, model: Option<&str>) -> Vec<String> {
        adapter
            .build_command(&request(model))
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn has_model_flag(args: &[String]) -> bool {
        args.iter().any(|a| a == "--model" || a == "-m")
    }

    #[test]
    fn explicit_model_is_none_for_the_cli_default() {
        assert_eq!(request(None).explicit_model(), None);
        assert_eq!(request(Some(CLI_DEFAULT_MODEL)).explicit_model(), None);
        assert_eq!(request(Some("gpt-x")).explicit_model(), Some("gpt-x"));
    }

    /// The `default` model defers to whatever the provider CLI itself would
    /// pick — the one choice that cannot go stale when a provider renames or
    /// retires a model, and that always fits the user's subscription tier.
    #[test]
    fn default_model_passes_no_model_flag_to_any_cli() {
        for adapter in all_adapters() {
            let args = args_for(adapter.as_ref(), Some(CLI_DEFAULT_MODEL));
            assert!(
                !has_model_flag(&args) && !args.iter().any(|a| a == CLI_DEFAULT_MODEL),
                "{} must not receive a model flag: {args:?}",
                adapter.cli_binary()
            );
        }
    }

    #[test]
    fn named_model_is_passed_to_every_cli() {
        for adapter in all_adapters() {
            let args = args_for(adapter.as_ref(), Some("some-model"));
            assert!(
                has_model_flag(&args) && args.iter().any(|a| a == "some-model"),
                "{} must receive the model: {args:?}",
                adapter.cli_binary()
            );
        }
    }
}
