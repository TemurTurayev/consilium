//! M3b localhost server: streams a run's `AgentEvent`s to a browser over a
//! WebSocket. A client opens `/ws/session`, sends one JSON frame describing the
//! run (`{ "kind": "conduct", "task": "...", "context": "...", "cwd": "..." }`),
//! and receives each event live as a text frame, then a terminal
//! `run_complete` / `run_error` frame, then close.
//!
//! Live delivery uses the engine's task-local [`ProgressSink`] (M3b1): the run
//! executes inside `PROGRESS_SINK.scope(sink, ...)`, the sink fans each event
//! into an unbounded channel, and a writer task drains it to the socket — so the
//! engine's collect loop never blocks on socket backpressure.

use crate::config::Config;
use crate::event::{AgentEvent, Provider};
use crate::orchestrator::conduct::{run_conduct, ConductDeps, RoleHandle};
use crate::orchestrator::council::CouncilMember;
use crate::orchestrator::progress::{ProgressSink, PROGRESS_SINK};
use crate::orchestrator::resilience::ModelHealth;
use crate::orchestrator::roles;
use crate::protocol::{ProviderUsage, QuotaSnapshot, ServerFrame, SessionRequest};
use crate::quota::QuotaStore;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Builds a fresh `ConductDeps` per run. The real server captures `Arc<Config>`
/// and resolves real CLI adapters; tests inject a closure returning scripted
/// ladders (so the WS path is exercised end-to-end at zero quota).
type DepsFn = dyn Fn() -> ConductDeps + Send + Sync;

#[derive(Clone)]
pub struct ServerState {
    deps_fn: Arc<DepsFn>,
    quota: Arc<QuotaStore>,
    timeout: Duration,
}

impl ServerState {
    /// Production: resolve `ConductDeps` from config on each run.
    pub fn from_config(config: Config, quota: QuotaStore, timeout: Duration) -> Self {
        let config = Arc::new(config);
        Self {
            deps_fn: Arc::new(move || build_conduct_deps(&config)),
            quota: Arc::new(quota),
            timeout,
        }
    }

    /// Tests: supply a `ConductDeps` builder (e.g. scripted adapters) directly.
    pub fn from_parts(deps_fn: Arc<DepsFn>, quota: QuotaStore, timeout: Duration) -> Self {
        Self {
            deps_fn,
            quota: Arc::new(quota),
            timeout,
        }
    }
}

/// Build the axum router. `ServerState` is cheap to clone (Arcs).
pub fn router(state: ServerState) -> Router {
    Router::new()
        .route("/ws/session", get(ws_session))
        .route("/api/quota", get(quota_handler))
        .with_state(state)
}

/// Serve on `addr` until the process is terminated.
pub async fn serve(
    addr: std::net::SocketAddr,
    config: Config,
    quota: QuotaStore,
    timeout: Duration,
) -> anyhow::Result<()> {
    let state = ServerState::from_config(config, quota, timeout);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "consilium server listening (ws: /ws/session)");
    axum::serve(listener, router(state)).await?;
    Ok(())
}

async fn ws_session(ws: WebSocketUpgrade, State(state): State<ServerState>) -> Response {
    ws.on_upgrade(move |socket| handle_session(socket, state))
}

/// `GET /api/quota` → current per-provider usage over the rolling window.
async fn quota_handler(State(state): State<ServerState>) -> Json<QuotaSnapshot> {
    Json(quota_snapshot(state.quota.as_ref()))
}

/// Build a usage snapshot over the rolling window. Extracted from the handler so
/// it's unit-testable without standing up the HTTP server.
pub fn quota_snapshot(quota: &QuotaStore) -> QuotaSnapshot {
    let since = crate::quota::unix_now() - crate::quota::WINDOW_SECS;
    let usage = |provider: Provider| -> ProviderUsage {
        let (input_tokens, output_tokens) = quota.totals_since(provider, since).unwrap_or((0, 0));
        ProviderUsage {
            input_tokens,
            output_tokens,
        }
    };
    QuotaSnapshot {
        window_secs: crate::quota::WINDOW_SECS,
        claude: usage(Provider::Claude),
        codex: usage(Provider::Codex),
        gemini: usage(Provider::Gemini),
    }
}

/// Forwards each engine event (as JSON) into a channel drained by the writer
/// task. `on_event` is non-blocking (a channel send), so the engine never stalls
/// on socket backpressure.
struct WsSink {
    tx: mpsc::UnboundedSender<String>,
}
impl ProgressSink for WsSink {
    fn on_event(&self, ev: &AgentEvent) {
        match serde_json::to_string(ev) {
            Ok(json) => {
                let _ = self.tx.send(json);
            }
            Err(e) => tracing::warn!(error = %e, "ws: failed to serialize event; dropping"),
        }
    }
}

async fn handle_session(socket: WebSocket, state: ServerState) {
    let (mut sender, mut receiver) = socket.split();

    // First frame describes the run.
    let req: SessionRequest = match receiver.next().await {
        Some(Ok(Message::Text(t))) => match serde_json::from_str(t.as_str()) {
            Ok(r) => r,
            Err(e) => {
                let frame = ServerFrame::Error {
                    error: format!("bad request: {e}"),
                };
                let _ = sender
                    .send(Message::Text(
                        serde_json::to_string(&frame).unwrap_or_default().into(),
                    ))
                    .await;
                return;
            }
        },
        _ => return, // socket closed or first frame was not text
    };

    // sink (sync on_event) → channel → writer task (async socket sends).
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let term_tx = tx.clone();
    let sink: Arc<dyn ProgressSink> = Arc::new(WsSink { tx });

    let writer = tokio::spawn(async move {
        while let Some(json) = rx.recv().await {
            if sender.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
        let _ = sender.send(Message::Close(None)).await;
    });

    match req {
        SessionRequest::Conduct { task, context, cwd } => {
            let cwd = cwd
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let deps = (state.deps_fn)();
            let health = ModelHealth::new();
            let result = PROGRESS_SINK
                .scope(sink, async {
                    run_conduct(
                        &task,
                        &context,
                        deps,
                        state.quota.as_ref(),
                        cwd,
                        state.timeout,
                        &health,
                    )
                    .await
                })
                .await;
            let frame = match &result {
                Ok(o) => ServerFrame::from(o),
                Err(e) => ServerFrame::RunError {
                    error: e.to_string(),
                },
            };
            let _ = term_tx.send(serde_json::to_string(&frame).unwrap_or_default());
        }
    }

    // Drop the last sender so the writer drains the terminal frame, then closes.
    drop(term_tx);
    let _ = writer.await;
}

/// Resolve `ConductDeps` from config (same wiring as `consilium conduct`).
fn build_conduct_deps(config: &Config) -> ConductDeps {
    let workers: Vec<CouncilMember> = config
        .roles
        .workers
        .iter()
        .map(|role| CouncilMember {
            label: format!("{}-{}", role.provider.as_str(), role.model),
            ladder: roles::resolve_ladder(role),
        })
        .collect();
    ConductDeps {
        conductor: RoleHandle {
            ladder: roles::resolve_ladder(&config.roles.conductor),
        },
        workers,
        supervisor: Some(RoleHandle {
            ladder: roles::resolve_ladder(&config.roles.supervisor),
        }),
        reviewer: Some(RoleHandle {
            ladder: roles::resolve_ladder(&config.roles.reviewer),
        }),
        arbiter: Some(RoleHandle {
            ladder: roles::resolve_ladder(&config.roles.chairman),
        }),
        verify: config.verify.clone(),
        memory: config.conductor_memory.clone().unwrap_or_default(),
        cross_family_review: config.cross_family_review,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_sink_forwards_serialized_events() {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let sink = WsSink { tx };
        sink.on_event(&AgentEvent::Message {
            text: "hello".into(),
        });
        let json = rx.try_recv().expect("sink should forward the event");
        assert!(json.contains("\"type\":\"message\""), "got {json}");
        assert!(json.contains("hello"), "got {json}");
    }

    #[test]
    fn quota_snapshot_reports_recorded_usage_per_provider() {
        let quota = QuotaStore::open_in_memory().unwrap();
        quota.record(Provider::Gemini, 100, 20).unwrap();
        quota.record(Provider::Gemini, 50, 10).unwrap();
        quota.record(Provider::Codex, 7, 3).unwrap();

        let snap = quota_snapshot(&quota);
        assert_eq!(snap.window_secs, crate::quota::WINDOW_SECS);
        assert_eq!(snap.gemini.input_tokens, 150);
        assert_eq!(snap.gemini.output_tokens, 30);
        assert_eq!(snap.codex.input_tokens, 7);
        assert_eq!(snap.claude.input_tokens, 0);
        assert_eq!(snap.claude.output_tokens, 0);
    }
}
