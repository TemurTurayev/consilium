//! The client-facing WebSocket protocol, in one place so it can be exported to
//! TypeScript via `ts-rs` as a single source of truth.
//!
//! Two layers share the on-wire `type` discriminant namespace but have distinct
//! owners and lifecycles, so they stay as sibling enums (the TS client unions
//! them as `InboundFrame = AgentEvent | ServerFrame`):
//!   * [`crate::event::AgentEvent`] — engine-owned live events, serialized
//!     verbatim by the server's `WsSink`.
//!   * [`ServerFrame`] — server-owned run-lifecycle frames (this module).
//!
//! [`SessionRequest`] is the single client→server frame (the first message).
//!
//! Every type here derives `ts_rs::TS` and exports to `ui/src/protocol/`
//! (`export_to` is relative to this source file's dir). Run `cargo test` to
//! regenerate; the generated `.ts` files are committed so the UI builds without
//! cargo and so protocol changes show up in diffs.

use crate::orchestrator::conduct::ConductOutcome;
use serde::{Deserialize, Serialize};

/// Server→client run-lifecycle frames. The tags are disjoint from every
/// [`AgentEvent`](crate::event::AgentEvent) tag, so a client can discriminate
/// the whole inbound stream on the single `type` field.
#[derive(Debug, Serialize, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub enum ServerFrame {
    /// Terminal frame for a run that finished (any accepted subtasks).
    RunComplete {
        /// Accepted subtask ids, in order.
        completed: Vec<u32>,
        /// Set if the supervisor halted the run.
        halted: Option<String>,
        /// Set if the conductor failed the run or rework was exhausted.
        failed: Option<String>,
    },
    /// Terminal frame for a run that errored before producing an outcome.
    RunError { error: String },
    /// The first frame failed to parse as a [`SessionRequest`].
    Error { error: String },
}

impl From<&ConductOutcome> for ServerFrame {
    fn from(o: &ConductOutcome) -> Self {
        ServerFrame::RunComplete {
            completed: o.completed.clone(),
            halted: o.halted.clone(),
            failed: o.failed.clone(),
        }
    }
}

/// Client→server request: the first frame on a `/ws/session` socket.
#[derive(Debug, Deserialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub enum SessionRequest {
    Conduct {
        task: String,
        #[serde(default)]
        context: String,
        /// Working directory the run edits; defaults to the server's cwd.
        #[serde(default)]
        cwd: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ts_rs::TS;

    #[test]
    fn run_complete_serializes_with_exact_tag_and_fields() {
        let f = ServerFrame::RunComplete {
            completed: vec![1, 2],
            halted: None,
            failed: None,
        };
        assert_eq!(
            serde_json::to_string(&f).unwrap(),
            r#"{"type":"run_complete","completed":[1,2],"halted":null,"failed":null}"#
        );
    }

    #[test]
    fn run_error_and_error_tags() {
        assert_eq!(
            serde_json::to_string(&ServerFrame::RunError {
                error: "boom".into()
            })
            .unwrap(),
            r#"{"type":"run_error","error":"boom"}"#
        );
        assert_eq!(
            serde_json::to_string(&ServerFrame::Error {
                error: "bad".into()
            })
            .unwrap(),
            r#"{"type":"error","error":"bad"}"#
        );
    }

    #[test]
    fn from_conduct_outcome_maps_fields() {
        let o = ConductOutcome {
            completed: vec![3],
            halted: Some("h".into()),
            failed: None,
            transcript: serde_json::Value::Null,
        };
        match ServerFrame::from(&o) {
            ServerFrame::RunComplete {
                completed,
                halted,
                failed,
            } => {
                assert_eq!(completed, vec![3]);
                assert_eq!(halted.as_deref(), Some("h"));
                assert_eq!(failed, None);
            }
            other => panic!("expected RunComplete, got {other:?}"),
        }
    }

    #[test]
    fn session_request_parses_and_rejects() {
        let r: SessionRequest =
            serde_json::from_str(r#"{"kind":"conduct","task":"t","context":"c"}"#).unwrap();
        let SessionRequest::Conduct { task, context, cwd } = r;
        assert_eq!(task, "t");
        assert_eq!(context, "c");
        assert!(cwd.is_none());
        assert!(serde_json::from_str::<SessionRequest>(r#"{"kind":"nope"}"#).is_err());
    }

    // Pins the wire-format invariants so a careless edit breaks the build:
    // the control frame keeps nullable halted/failed (string | null).
    #[test]
    fn ts_control_frame_keeps_nullable_fields() {
        let decl = ServerFrame::decl(&Default::default());
        assert!(decl.contains("halted: string | null"), "decl: {decl}");
        assert!(decl.contains("failed: string | null"), "decl: {decl}");
    }
}
