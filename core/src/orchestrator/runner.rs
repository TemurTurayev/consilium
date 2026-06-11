use crate::adapters::{Adapter, RunRequest};
use crate::event::AgentEvent;
use crate::quota::QuotaStore;
use crate::sessions;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub enum RunStatus {
    Completed,
    Failed(String),
    TimedOut,
}

#[derive(Debug)]
pub struct RunOutcome {
    pub session_id: String,
    /// Completed.result if present, else the last Message text, else empty.
    pub final_text: String,
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

    // Collect returns (events, terminal_status) to avoid borrow-checker issues
    // with capturing &mut locals across the async boundary.
    let collect = async move {
        let mut events: Vec<AgentEvent> = Vec::new();
        let mut status: Option<RunStatus> = None;

        while let Some(ev) = handle.events.recv().await {
            match &ev {
                AgentEvent::Usage {
                    input_tokens,
                    output_tokens,
                } => {
                    quota.record(provider, *input_tokens, *output_tokens)?;
                }
                AgentEvent::Completed { .. } if status.is_none() => {
                    status = Some(RunStatus::Completed);
                }
                AgentEvent::Failed { error } if status.is_none() => {
                    status = Some(RunStatus::Failed(error.clone()));
                }
                _ => {}
            }
            events.push(ev);
        }
        anyhow::Ok((events, status))
    };

    let result = tokio::time::timeout(timeout, collect).await;

    let (events, status) = match result {
        Err(_elapsed) => {
            // Timeout: child is orphaned per M1 policy.
            return Ok(RunOutcome {
                session_id,
                final_text: String::new(),
                events: Vec::new(),
                status: RunStatus::TimedOut,
            });
        }
        Ok(Err(e)) => return Err(e),
        Ok(Ok(pair)) => pair,
    };

    let status =
        status.unwrap_or_else(|| RunStatus::Failed("stream ended without terminal event".into()));

    let final_text = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Completed { result: Some(r) } => Some(r.clone()),
            _ => None,
        })
        .or_else(|| {
            events.iter().rev().find_map(|e| match e {
                AgentEvent::Message { text } => Some(text.clone()),
                _ => None,
            })
        })
        .unwrap_or_default();

    Ok(RunOutcome {
        session_id,
        final_text,
        events,
        status,
    })
}
