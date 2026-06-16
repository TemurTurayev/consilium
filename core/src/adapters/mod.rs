pub mod claude;
pub mod codex;
pub mod gemini;

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
    /// CLI's scoped auto-approve-edits flag (verified 2026-06-12):
    /// claude `--permission-mode acceptEdits`, codex `--sandbox workspace-write`,
    /// gemini `--approval-mode auto_edit`. Deliberation runs keep this false —
    /// council/review must never mutate files.
    ///
    /// INVARIANT: `advisory` and `write` must not both be true (enforced by a
    /// hard `bail!` in sessions::spawn — real in release builds too). Design
    /// note: two orthogonal bools
    /// rather than a RunMode enum — they govern independent provider behaviors;
    /// an enum would need three variants to preserve the advisory-only case.
    pub write: bool,
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
