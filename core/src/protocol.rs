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

use crate::config::Config;
use crate::event::Provider;
use crate::orchestrator::conduct::ConductOutcome;
use serde::{Deserialize, Serialize};

/// Server→client run-lifecycle frames. The tags are disjoint from every
/// [`AgentEvent`](crate::event::AgentEvent) tag, so a client can discriminate
/// the whole inbound stream on the single `type` field (pinned by the
/// `server_frame_tags_disjoint_from_agent_event_tags` test below).
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
    /// Terminal frame for a run aborted by the client (cancel frame or the
    /// socket closing). Agent child processes are killed before this is sent.
    RunCancelled {},
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

/// Per-provider token usage over the reporting window.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct ProviderUsage {
    #[ts(type = "number")]
    pub input_tokens: u64,
    #[ts(type = "number")]
    pub output_tokens: u64,
    /// True when these tokens are heuristic estimates (the provider's CLI reports
    /// no usage, e.g. Gemini via the Antigravity `agy` CLI) rather than measured.
    pub estimated: bool,
}

/// A snapshot of quota usage per provider over the rolling window — served at
/// `GET /api/quota` for the dashboard.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct QuotaSnapshot {
    /// The window length the totals cover, in seconds.
    #[ts(type = "number")]
    pub window_secs: i64,
    pub claude: ProviderUsage,
    pub codex: ProviderUsage,
    pub gemini: ProviderUsage,
    pub grok: ProviderUsage,
}

/// Client→server frames on a `/ws/session` socket: the first frame describes
/// the run; `{"kind":"cancel"}` at any later point aborts it.
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
    /// Abort the active run (server replies with a terminal `run_cancelled`).
    Cancel,
    /// Operator (chief physician) control: park the active run at its next
    /// boundary (top of the next subtask dispatch) until `Resume` or `Cancel`.
    /// As the FIRST frame on a socket (no run active yet) this is invalid,
    /// same as `Cancel`, and gets the same structured `error` reply.
    Pause,
    /// Release a paused run so it continues past its parked boundary.
    Resume,
    /// Queue an operator note; it reaches the conductor's next decision
    /// (evaluation/replan) and is echoed back as an `operator_note` event.
    Interject { text: String },
}

/// One provider's auth/liveness state — the wire shape of
/// [`crate::auth::ProviderAuth`] for `GET /api/doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub enum AuthState {
    Ready,
    NeedsLogin,
    CliMissing,
    Down,
}

/// One provider row in the doctor report.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct ProviderStatus {
    pub provider: Provider,
    pub state: AuthState,
    /// Probe failure detail (empty when ready).
    pub detail: String,
    /// One-line actionable next step (e.g. the exact login command).
    pub hint: String,
}

/// `GET /api/doctor` — live auth/liveness report for every provider.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct DoctorReport {
    pub providers: Vec<ProviderStatus>,
}

/// `GET /api/config` — a read-only summary of the council the server runs
/// with. Roles are `provider/model` strings for display, not for editing.
#[derive(Debug, Clone, Default, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct ConfigSummary {
    pub conductor: String,
    pub workers: Vec<String>,
    pub reviewer: String,
    pub chairman: String,
    pub supervisor: String,
    pub cross_family_review: bool,
    #[ts(type = "number | null")]
    pub budget_secs: Option<u64>,
    /// Where the config was loaded from (`null` = built-in defaults).
    pub config_path: Option<String>,
}

impl ConfigSummary {
    pub fn from_config(config: &Config, config_path: Option<String>) -> Self {
        let role = |r: &crate::config::RoleConfig| format!("{}/{}", r.provider.as_str(), r.model);
        ConfigSummary {
            conductor: role(&config.roles.conductor),
            workers: config.roles.workers.iter().map(role).collect(),
            reviewer: role(&config.roles.reviewer),
            chairman: role(&config.roles.chairman),
            supervisor: role(&config.roles.supervisor),
            cross_family_review: config.cross_family_review,
            budget_secs: config.budget_secs,
            config_path,
        }
    }
}

/// `GET /api/version` — the server's crate version.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct VersionInfo {
    pub version: String,
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
        let SessionRequest::Conduct { task, context, cwd } = r else {
            panic!("expected Conduct");
        };
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

    #[test]
    fn run_cancelled_serializes_with_exact_tag() {
        assert_eq!(
            serde_json::to_string(&ServerFrame::RunCancelled {}).unwrap(),
            r#"{"type":"run_cancelled"}"#
        );
    }

    #[test]
    fn cancel_request_parses() {
        assert!(matches!(
            serde_json::from_str::<SessionRequest>(r#"{"kind":"cancel"}"#).unwrap(),
            SessionRequest::Cancel
        ));
    }

    #[test]
    fn pause_resume_interject_requests_parse() {
        assert!(matches!(
            serde_json::from_str::<SessionRequest>(r#"{"kind":"pause"}"#).unwrap(),
            SessionRequest::Pause
        ));
        assert!(matches!(
            serde_json::from_str::<SessionRequest>(r#"{"kind":"resume"}"#).unwrap(),
            SessionRequest::Resume
        ));
        let SessionRequest::Interject { text } =
            serde_json::from_str::<SessionRequest>(r#"{"kind":"interject","text":"slow down"}"#)
                .unwrap()
        else {
            panic!("expected Interject");
        };
        assert_eq!(text, "slow down");
    }

    /// Every tag value in a TS decl of a `#[serde(tag = "type")]` enum, e.g.
    /// `type: "run_complete"` → `run_complete`.
    fn tags_of(decl: &str) -> Vec<String> {
        decl.split("\"type\": \"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next().map(str::to_string))
            .collect()
    }

    /// The whole client discriminates `InboundFrame = AgentEvent | ServerFrame`
    /// on the single `type` field — a shared tag would silently misroute frames.
    #[test]
    fn server_frame_tags_disjoint_from_agent_event_tags() {
        let server = tags_of(&ServerFrame::decl(&Default::default()));
        let agent = tags_of(&crate::event::AgentEvent::decl(&Default::default()));
        assert!(
            !server.is_empty() && !agent.is_empty(),
            "tag extraction broke"
        );
        for tag in &server {
            assert!(!agent.contains(tag), "tag '{tag}' exists in both enums");
        }
    }

    #[test]
    fn config_summary_formats_roles() {
        let s = ConfigSummary::from_config(&Config::default(), Some("x.json".into()));
        assert!(s.conductor.contains('/'), "got: {}", s.conductor);
        assert!(!s.workers.is_empty());
        assert_eq!(s.config_path.as_deref(), Some("x.json"));
    }
}
