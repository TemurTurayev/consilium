pub mod claude;
pub mod codex;
// pub mod gemini;  // Task 6

use crate::event::{AgentEvent, Provider};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub cwd: PathBuf,
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
}
