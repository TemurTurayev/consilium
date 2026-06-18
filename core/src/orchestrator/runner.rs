use crate::adapters::{Adapter, RunRequest};
use crate::event::AgentEvent;
use crate::orchestrator::progress;
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
/// design note); a timeout aborts the reader task, which SIGKILLs the child
/// (kill_on_drop) so it can't keep mutating the cwd after we move on.
pub async fn run_to_completion(
    adapter: Arc<dyn Adapter>,
    req: RunRequest,
    quota: &QuotaStore,
    timeout: Duration,
) -> anyhow::Result<RunOutcome> {
    let provider = adapter.provider();
    let sessions::SessionHandle {
        id: session_id,
        events: mut event_rx,
        task,
    } = sessions::spawn(adapter, req)?;

    // Collect returns (events, terminal_status, final_text) to avoid
    // borrow-checker issues with capturing &mut locals across the async boundary.
    let collect = async move {
        let mut events: Vec<AgentEvent> = Vec::new();
        let mut status: Option<RunStatus> = None;
        // Outer Option = saw the first terminal Completed; inner = it carried a result.
        let mut final_text_candidate: Option<Option<String>> = None;
        let mut last_message: Option<String> = None;

        while let Some(ev) = event_rx.recv().await {
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
            // Live tap: forward each event to the task-local progress sink as it
            // arrives (M3b). No-op when no sink is installed (CLI/tests) → behavior
            // identical to before. Kept before the move-push below.
            progress::emit(&ev);
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
            // Timeout: abort the reader task and AWAIT its cancellation. Dropping
            // the task drops the child (spawned kill_on_drop), so SIGKILL is issued
            // BEFORE we return — synchronously here, not merely scheduled — so a
            // hung/slow write worker is terminated, not left mutating the shared
            // cwd while conduct starts the next attempt in the same directory.
            task.abort();
            let _ = task.await;
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
