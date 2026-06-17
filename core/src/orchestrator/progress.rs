//! Live progress sink (M3b): a transport-agnostic tap that fires once per
//! `AgentEvent` as a session streams, so a server can forward events to a
//! browser in real time.
//!
//! ## Why a task-local, not threaded parameters
//!
//! The sink is ambient run context, not data the engine computes with — so it is
//! carried in a `tokio` task-local rather than threaded through every
//! orchestration signature. The CLI and all tests install no sink, so
//! [`emit`] is a no-op and their behavior is byte-identical to before M3b. A
//! server installs one via [`PROGRESS_SINK`]`.scope(...)` around the run.
//!
//! The read happens inside `run_to_completion`'s event-collection loop, which is
//! awaited inline within the run future (no `tokio::spawn` between the server's
//! `scope` and the loop), so the task-local is in scope exactly where events
//! arrive. (Sequential `conduct` is fully covered; parallel council members that
//! are `tokio::spawn`ed would not inherit the task-local — out of scope for the
//! conduct-first server slice.)

use crate::event::AgentEvent;
use std::sync::Arc;

/// Receives each `AgentEvent` as it arrives from a running session.
///
/// Implementors must be cheap and non-blocking: `on_event` is called inline on
/// the event-collection path, so a slow sink stalls the bounded session channel
/// (cooperative backpressure onto the worker's stdout). A WebSocket sink should
/// hand the event to an owned buffer/channel and return immediately.
pub trait ProgressSink: Send + Sync {
    fn on_event(&self, event: &AgentEvent);
}

tokio::task_local! {
    /// The progress sink installed for the current run. A server wraps the run
    /// in `PROGRESS_SINK.scope(sink, fut)`; unset elsewhere (CLI/tests).
    pub static PROGRESS_SINK: Arc<dyn ProgressSink>;
}

/// Forward one event to the task-local sink if one is installed for this run.
/// No-op (never panics) when no sink is in scope — the engine never depends on
/// streaming being present.
pub fn emit(event: &AgentEvent) {
    let _ = PROGRESS_SINK.try_with(|sink| sink.on_event(event));
}
