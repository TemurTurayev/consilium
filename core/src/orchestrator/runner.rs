use crate::adapters::{Adapter, RunRequest};
use crate::event::AgentEvent;
use crate::quota::QuotaStore;
use crate::sessions;
use std::sync::Arc;
use std::time::Duration;

// PartialEq: forward-declared for orchestrator tests (council/review) that compare statuses.
#[derive(Debug, Clone, PartialEq)]
pub enum RunStatus {
    Completed,
    Failed(String),
    TimedOut,
}

#[derive(Debug)]
pub struct RunOutcome {
    pub session_id: String,
    /// Result of the FIRST terminal Completed event if it carried one, else
    /// the last Message text, else empty.
    pub final_text: String,
    /// All events collected during the session. Empty when status is TimedOut
    /// (the future is dropped on timeout — collected events are lost).
    pub events: Vec<AgentEvent>,
    pub status: RunStatus,
}

/// Drives one agent session to completion: collects all events, records Usage
/// into the quota store, derives the final text, and applies a hard timeout.
/// First terminal event (Completed/Failed) is authoritative (see sessions.rs
/// design note); a timeout abandons the stream (child is orphaned — M1 policy).
pub async fn run_to_completion(
    adapter: Arc<dyn Adapter>,
    req: RunRequest,
    quota: &QuotaStore,
    timeout: Duration,
) -> anyhow::Result<RunOutcome> {
    let provider = adapter.provider();
    let mut handle = sessions::spawn(adapter, req)?;
    let session_id = handle.id.clone();

    // Collect returns (events, terminal_status, final_text) to avoid
    // borrow-checker issues with capturing &mut locals across the async boundary.
    let collect = async move {
        let mut events: Vec<AgentEvent> = Vec::new();
        let mut status: Option<RunStatus> = None;
        // Outer Option = saw the first terminal Completed; inner = it carried a result.
        let mut final_text_candidate: Option<Option<String>> = None;
        let mut last_message: Option<String> = None;

        while let Some(ev) = handle.events.recv().await {
            match &ev {
                AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                } => {
                    // Accounting is a side-channel: a failed write must not abort collection.
                    if let Err(e) = quota.record(provider, *input_tokens, *output_tokens) {
                        tracing::warn!(error = %e, "quota record failed; continuing");
                    }
                }
                AgentEvent::Message { text } => {
                    last_message = Some(text.clone());
                }
                AgentEvent::Completed { result } if status.is_none() => {
                    final_text_candidate = Some(result.clone());
                    status = Some(RunStatus::Completed);
                }
                AgentEvent::Failed { error } if status.is_none() => {
                    status = Some(RunStatus::Failed(error.clone()));
                }
                _ => {}
            }
            events.push(ev);
        }

        let final_text = final_text_candidate
            .unwrap_or(None)
            .or(last_message)
            .unwrap_or_default();
        (events, status, final_text)
    };

    let (events, status, final_text) = match tokio::time::timeout(timeout, collect).await {
        Err(_elapsed) => {
            // Timeout: child is orphaned per M1 policy (not killed). M2b WRITE
            // HAZARD: an orphaned worker run keeps its scoped write flag active
            // and can keep mutating the shared cwd while conduct starts the next
            // attempt — a concurrent-write race. TODO(M3): start_kill()+wait()
            // the child on timeout for write runs before proceeding.
            return Ok(RunOutcome {
                session_id,
                final_text: String::new(),
                events: Vec::new(),
                status: RunStatus::TimedOut,
            });
        }
        Ok(collected) => collected,
    };

    let status =
        status.unwrap_or_else(|| RunStatus::Failed("stream ended without terminal event".into()));

    Ok(RunOutcome {
        session_id,
        final_text,
        events,
        status,
    })
}
