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
    /// The provider's CLI is not signed in. Neither a retry nor another model
    /// of the same provider can succeed — the whole provider is skipped for
    /// the rest of the run and the operator is told to sign in.
    Unauthenticated,
}

/// An adapter knows how to launch one provider's CLI and translate its raw
/// output into AgentEvents. Parsing is PURE (no I/O) so it is fixture-testable.
pub trait Adapter: Send + Sync {
    fn provider(&self) -> Provider;
    fn cli_binary(&self) -> &'static str;
    /// Environment variables this CLI signs in with. When one is missing from
    /// the environment, `sessions::spawn` supplies it from
    /// `~/.consilium/secrets.env` (see `crate::secrets`). Default: none — the
    /// CLI keeps its own login.
    fn credential_env(&self) -> &'static [&'static str] {
        &[]
    }
    /// What this CLI prints on stderr when, not signed in, it stops to wait for
    /// a sign-in in the browser instead of failing. A headless run can never
    /// complete that sign-in, so `sessions::spawn` ends the run the moment one
    /// of these appears rather than waiting out the CLI (agy: 60 s, grok: for
    /// ever). The CLI's own words, exactly as printed. Default: none — the CLI
    /// fails by itself.
    fn sign_in_prompts(&self) -> &'static [&'static str] {
        &[]
    }
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

/// What introduces a CLI's last stderr output in the failure of a run that
/// exited non-zero (see `sessions`).
pub(crate) const STDERR_TAIL_LABEL: &str = "stderr tail: ";

/// The failure a run ends with when its CLI stopped to wait for a sign-in.
pub fn sign_in_failure(cli: &str, prompt: &str) -> String {
    format!(
        "{cli} is not signed in — it stopped to wait for a sign-in in the browser (\"{prompt}\")"
    )
}

/// Whether a run's failure says `cli` asked for a sign-in: it was stopped for
/// asking ([`sign_in_failure`]), or it gave up by itself and its stderr opens
/// with one of `prompts`. The same words anywhere else are something a
/// working CLI logged — a worker editing this very code, say — not a sign-out.
pub(crate) fn asked_to_sign_in(cli: &str, prompts: &[&str], error: &str) -> bool {
    prompts.iter().any(|prompt| {
        error.contains(&sign_in_failure(cli, prompt))
            || error.contains(&format!("{STDERR_TAIL_LABEL}{prompt}"))
    })
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

    /// Claude's headless CLI signs in with the token `claude setup-token`
    /// prints; the other CLIs keep their own login and need nothing handed in.
    #[test]
    fn only_claude_asks_for_a_saved_credential() {
        assert_eq!(
            claude::ClaudeAdapter.credential_env(),
            ["CLAUDE_CODE_OAUTH_TOKEN"]
        );
        for adapter in [
            Arc::new(codex::CodexAdapter) as Arc<dyn Adapter>,
            Arc::new(gemini::GeminiAdapter),
            Arc::new(grok::GrokAdapter),
        ] {
            assert!(
                adapter.credential_env().is_empty(),
                "{} must not ask for a saved credential",
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

    /// agy and grok, when not signed in, stop and wait for a sign-in in the
    /// browser instead of failing (agy 1.5.14, grok 0.2.87 — recorded
    /// 2026-11-22); claude and codex fail by themselves.
    #[test]
    fn only_clis_that_wait_for_a_browser_sign_in_name_their_prompt() {
        assert!(claude::ClaudeAdapter.sign_in_prompts().is_empty());
        assert!(codex::CodexAdapter.sign_in_prompts().is_empty());
        assert!(!gemini::GeminiAdapter.sign_in_prompts().is_empty());
        assert!(!grok::GrokAdapter.sign_in_prompts().is_empty());
    }

    #[test]
    fn a_sign_in_failure_names_the_cli_and_quotes_its_prompt() {
        let failure = sign_in_failure("agy", "Authentication required.");
        assert!(failure.starts_with("agy is not signed in"), "{failure}");
        assert!(
            failure.contains("\"Authentication required.\""),
            "{failure}"
        );
    }

    const PROMPT: &str = "To sign in, open this URL";

    #[test]
    fn the_failure_of_a_cli_stopped_for_asking_says_it_asked_to_sign_in() {
        let failure = sign_in_failure("grok", PROMPT);
        assert!(asked_to_sign_in("grok", &[PROMPT], &failure));
    }

    #[test]
    fn an_exit_whose_stderr_opens_with_the_prompt_asked_to_sign_in() {
        let failure = format!(
            "process exited with exit status: 1; {STDERR_TAIL_LABEL}{PROMPT}:\n  https://example.test"
        );
        assert!(asked_to_sign_in("grok", &[PROMPT], &failure));
    }

    #[test]
    fn the_prompt_quoted_later_in_stderr_is_no_sign_out() {
        let failure = format!(
            "process exited with exit status: 1; {STDERR_TAIL_LABEL}edited grok.rs: \"{PROMPT}\"\nerror: tool failed"
        );
        assert!(!asked_to_sign_in("grok", &[PROMPT], &failure));
    }

    #[test]
    fn other_failures_and_clis_that_name_no_prompt_never_asked() {
        assert!(!asked_to_sign_in(
            "grok",
            &[PROMPT],
            "connection reset by peer"
        ));
        let failure = sign_in_failure("codex", PROMPT);
        assert!(!asked_to_sign_in("codex", &[], &failure));
    }
}
