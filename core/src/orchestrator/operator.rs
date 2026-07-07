//! Operator controls (M-ops): pause / resume / interject a running `conduct`,
//! mirroring the [`crate::orchestrator::progress`] task-local pattern.
//!
//! ## Why a task-local, not threaded parameters
//!
//! Like the progress sink, operator control is ambient run context supplied by
//! a server, not data the engine computes with — so it's carried in a `tokio`
//! task-local rather than threaded through every orchestration signature. The
//! CLI, the MCP server, `eval`, and every existing test install no handle, so
//! [`checkpoint`] is a no-op (returns no notes, never parks) and their
//! behavior is byte-identical to before this feature existed. A server
//! installs one handle per run via [`OPERATOR_CONTROLS`]`.scope(...)`, wrapped
//! around the same future as [`crate::orchestrator::progress::PROGRESS_SINK`].
//!
//! ## Boundary, not interruption
//!
//! `conduct::run_conduct` awaits [`checkpoint`] once per subtask, at the TOP
//! of the per-subtask dispatch loop — i.e. it never interrupts a live worker
//! call. Pausing mid-attempt lets that attempt (and its conductor
//! evaluation/rework) run to completion; the run only parks once it reaches
//! the next subtask's dispatch. Cancellation is unaffected: the server aborts
//! the whole run task regardless of whether it's parked inside
//! [`checkpoint`]'s resume-wait or mid-call.
//!
//! ## Pause/resume: a `watch<bool>`, not an atomic + `Notify`
//!
//! A `watch::Sender<bool>` doubles as both the paused flag (read via
//! `borrow()`) and the resume signal (`Receiver::wait_for`), which is
//! level-triggered: a rapid pause→resume→pause sequence with no intervening
//! [`checkpoint`] call always leaves the correct final state, unlike a plain
//! `Notify::notify_one`, whose single stored permit could be consumed by a
//! LATER, unrelated pause cycle (a phantom early resume). `wait_for` also
//! checks the current value before ever awaiting a change, so there is no
//! missed-wakeup window between the paused check and the wait.

use crate::event::AgentEvent;
use crate::orchestrator::progress;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};

struct Inner {
    paused: watch::Sender<bool>,
    /// Keeps the `paused` watch channel's receiver count above zero for the
    /// life of this handle. `watch::Sender::send` silently no-ops (returns
    /// `Err`, and — critically — never updates the stored value) once every
    /// receiver has been dropped; `checkpoint`'s `wait_for_resume` subscribes
    /// its own short-lived receiver each call, so without this permanent one
    /// a `pause()`/`resume()` racing between two checkpoints (receiver count
    /// transiently zero) would be silently dropped. Never read directly —
    /// state is read via `paused.borrow()` / `paused.subscribe()`.
    _paused_rx: watch::Receiver<bool>,
    notes_tx: mpsc::UnboundedSender<String>,
    notes_rx: Mutex<mpsc::UnboundedReceiver<String>>,
}

/// Per-run operator control handle. Cheap to clone (`Arc`-backed) — the
/// server creates one per run, installs it via [`OPERATOR_CONTROLS`]`.scope`,
/// and calls `pause`/`resume`/`interject` from the WS request-handling loop
/// as `SessionRequest::{Pause,Resume,Interject}` frames arrive.
#[derive(Clone)]
pub struct OperatorHandle {
    inner: Arc<Inner>,
}

impl Default for OperatorHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl OperatorHandle {
    pub fn new() -> Self {
        let (paused, paused_rx) = watch::channel(false);
        let (notes_tx, notes_rx) = mpsc::unbounded_channel();
        Self {
            inner: Arc::new(Inner {
                paused,
                _paused_rx: paused_rx,
                notes_tx,
                notes_rx: Mutex::new(notes_rx),
            }),
        }
    }

    /// `SessionRequest::Pause`: the run parks at its next boundary. Idempotent.
    pub fn pause(&self) {
        let _ = self.inner.paused.send(true);
    }

    /// `SessionRequest::Resume`: release a parked boundary. Safe to call when
    /// nothing is paused (a no-op on the paused state; sends no stray wake
    /// that could resolve a later, unrelated pause early).
    pub fn resume(&self) {
        let _ = self.inner.paused.send(false);
    }

    /// `SessionRequest::Interject`: queue a note for the next boundary to drain.
    pub fn interject(&self, text: String) {
        // An unbounded send only fails if the receiver was dropped, which
        // cannot happen while this handle (sharing the same `Arc<Inner>`) is
        // alive — the run task that would drop it also drops its own clone.
        let _ = self.inner.notes_tx.send(text);
    }

    fn is_paused(&self) -> bool {
        *self.inner.paused.borrow()
    }

    /// Drain every note queued since the last checkpoint. Never blocks.
    async fn drain_notes(&self) -> Vec<String> {
        let mut rx = self.inner.notes_rx.lock().await;
        let mut out = Vec::new();
        while let Ok(note) = rx.try_recv() {
            out.push(note);
        }
        out
    }

    /// Block until `paused` reads false. Race-free: `wait_for` checks the
    /// CURRENT value first, so a `resume()` that already landed before this
    /// call resolves immediately rather than waiting for a fresh change.
    async fn wait_for_resume(&self) {
        let mut rx = self.inner.paused.subscribe();
        let _ = rx.wait_for(|paused| !*paused).await;
    }
}

tokio::task_local! {
    /// The operator handle installed for the current run. A server wraps the
    /// run in `OPERATOR_CONTROLS.scope(handle, fut)`, alongside `PROGRESS_SINK`;
    /// unset elsewhere (CLI/MCP/eval/tests that don't opt in).
    pub static OPERATOR_CONTROLS: OperatorHandle;
}

/// Called by `run_conduct` at the top of each subtask's dispatch (before that
/// subtask's first worker attempt is launched — never mid-call). Drains and
/// returns any operator notes queued since the last checkpoint, then — if the
/// run is currently paused — emits `AgentEvent::Paused`, blocks until resumed,
/// and emits `AgentEvent::Resumed` (each exactly once per pause/resume pair).
///
/// No-op when no [`OperatorHandle`] is installed for this run: returns an
/// empty `Vec` immediately, never parks. This is the ONLY behavior this
/// module has for the CLI, the MCP server, `eval`, and every test that
/// doesn't install a scope — zero change from before this feature existed.
pub async fn checkpoint() -> Vec<String> {
    let Ok(handle) = OPERATOR_CONTROLS.try_with(|h| h.clone()) else {
        return Vec::new();
    };

    let notes = handle.drain_notes().await;

    if handle.is_paused() {
        progress::emit(&AgentEvent::Paused {});
        handle.wait_for_resume().await;
        progress::emit(&AgentEvent::Resumed {});
    }

    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    #[tokio::test]
    async fn checkpoint_without_scope_is_a_no_op() {
        // No OPERATOR_CONTROLS.scope installed — checkpoint must return
        // immediately with no notes and never park. A timeout would fail this
        // test if it parked.
        let notes = tokio::time::timeout(Duration::from_millis(200), checkpoint())
            .await
            .expect("checkpoint() must not block without an installed handle");
        assert!(notes.is_empty());
    }

    #[tokio::test]
    async fn interject_before_checkpoint_is_drained_once() {
        let handle = OperatorHandle::new();
        handle.interject("first".into());
        handle.interject("second".into());

        let notes = OPERATOR_CONTROLS.scope(handle.clone(), checkpoint()).await;
        assert_eq!(notes, vec!["first".to_string(), "second".to_string()]);

        // A second checkpoint with nothing newly queued drains nothing —
        // notes are consumed exactly once, not re-delivered.
        let notes2 = OPERATOR_CONTROLS.scope(handle, checkpoint()).await;
        assert!(notes2.is_empty());
    }

    #[tokio::test]
    async fn pause_blocks_checkpoint_until_resume() {
        let handle = OperatorHandle::new();
        handle.pause();

        let handle_for_run = handle.clone();
        let run = tokio::spawn(OPERATOR_CONTROLS.scope(handle_for_run, checkpoint()));

        // Give the spawned task a moment to reach the pause and start
        // waiting, then confirm it has NOT completed yet.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!run.is_finished(), "checkpoint must park while paused");

        handle.resume();
        let notes = tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .expect("checkpoint must complete promptly after resume")
            .unwrap();
        assert!(notes.is_empty());
    }

    #[tokio::test]
    async fn pause_emits_paused_and_resumed_exactly_once_each() {
        let events: Arc<StdMutex<Vec<AgentEvent>>> = Arc::new(StdMutex::new(Vec::new()));
        struct VecSink(Arc<StdMutex<Vec<AgentEvent>>>);
        impl progress::ProgressSink for VecSink {
            fn on_event(&self, event: &AgentEvent) {
                self.0.lock().unwrap().push(event.clone());
            }
        }
        let sink: Arc<dyn progress::ProgressSink> = Arc::new(VecSink(events.clone()));

        let handle = OperatorHandle::new();
        handle.pause();
        let handle_for_run = handle.clone();
        let run = tokio::spawn(OPERATOR_CONTROLS.scope(
            handle_for_run,
            progress::PROGRESS_SINK.scope(sink, checkpoint()),
        ));

        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.resume();
        tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .expect("checkpoint must complete promptly after resume")
            .unwrap();

        let recorded = events.lock().unwrap();
        let paused_count = recorded
            .iter()
            .filter(|e| matches!(e, AgentEvent::Paused {}))
            .count();
        let resumed_count = recorded
            .iter()
            .filter(|e| matches!(e, AgentEvent::Resumed {}))
            .count();
        assert_eq!(paused_count, 1, "got: {recorded:?}");
        assert_eq!(resumed_count, 1, "got: {recorded:?}");
    }

    #[tokio::test]
    async fn resume_before_pause_observed_does_not_leak_a_phantom_wake() {
        // pause() then resume() then pause() again, all before any
        // checkpoint() call ever waits — the watch channel's LEVEL (not an
        // edge count) must be the truth: the handle should still read paused,
        // and a checkpoint() call must genuinely block until a REAL resume().
        let handle = OperatorHandle::new();
        handle.pause();
        handle.resume();
        handle.pause();
        assert!(handle.is_paused());

        let handle_for_run = handle.clone();
        let run = tokio::spawn(OPERATOR_CONTROLS.scope(handle_for_run, checkpoint()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !run.is_finished(),
            "a stale permit from the earlier resume() must not unblock this pause cycle"
        );

        handle.resume();
        tokio::time::timeout(Duration::from_secs(2), run)
            .await
            .expect("checkpoint must complete promptly after the real resume()")
            .unwrap();
    }

    #[tokio::test]
    async fn resume_without_pause_is_harmless() {
        let handle = OperatorHandle::new();
        handle.resume(); // nothing paused — must not panic or misbehave
        assert!(!handle.is_paused());
        let notes = OPERATOR_CONTROLS.scope(handle, checkpoint()).await;
        assert!(notes.is_empty());
    }
}
