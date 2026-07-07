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
use crate::confine::cwd_within_root;
use crate::event::{AgentEvent, Provider};
use crate::orchestrator::conduct::{run_conduct, ConductDeps, RoleHandle};
use crate::orchestrator::council::CouncilMember;
use crate::orchestrator::progress::{ProgressSink, PROGRESS_SINK};
use crate::orchestrator::resilience::ModelHealth;
use crate::orchestrator::roles;
use crate::protocol::{
    AuthState, ConfigSummary, DoctorReport, ProviderStatus, ProviderUsage, QuotaSnapshot,
    ServerFrame, SessionRequest, VersionInfo,
};
use crate::quota::QuotaStore;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
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
    /// The directory `consilium serve` was launched in. Client-supplied `cwd`
    /// values are validated to be within this root before any run is started.
    launch_root: Arc<PathBuf>,
    /// Read-only council summary served at `GET /api/config`.
    config_summary: Arc<ConfigSummary>,
    /// One run at a time per server: concurrent conducts in one launch root
    /// would attribute each other's git diffs to the wrong run.
    active_run: Arc<tokio::sync::Semaphore>,
}

impl ServerState {
    /// Production: resolve `ConductDeps` from config on each run. The
    /// `launch_root` is captured once at startup from the process working dir.
    pub fn from_config(config: Config, quota: QuotaStore, timeout: Duration) -> Self {
        let summary = ConfigSummary::from_config(&config, None);
        let config = Arc::new(config);
        Self {
            deps_fn: Arc::new(move || build_conduct_deps(&config)),
            quota: Arc::new(quota),
            timeout,
            launch_root: Arc::new(std::env::current_dir().unwrap_or_default()),
            config_summary: Arc::new(summary),
            active_run: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    /// Tests: supply a `ConductDeps` builder (e.g. scripted adapters) directly.
    /// The `launch_root` defaults to the process cwd; pass a custom one via
    /// [`ServerState::with_launch_root`] when the test uses a temp dir as cwd.
    pub fn from_parts(deps_fn: Arc<DepsFn>, quota: QuotaStore, timeout: Duration) -> Self {
        Self {
            deps_fn,
            quota: Arc::new(quota),
            timeout,
            launch_root: Arc::new(std::env::current_dir().unwrap_or_default()),
            config_summary: Arc::new(ConfigSummary::default()),
            active_run: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }

    /// Override the launch root (used in tests that run agents in a temp dir,
    /// and by the desktop app where the root is the user-chosen workspace).
    pub fn with_launch_root(mut self, root: PathBuf) -> Self {
        self.launch_root = Arc::new(root);
        self
    }

    /// Record where the config was loaded from, for `GET /api/config`.
    pub fn with_config_path(mut self, path: Option<String>) -> Self {
        let mut summary = (*self.config_summary).clone();
        summary.config_path = path;
        self.config_summary = Arc::new(summary);
        self
    }
}

/// Build the axum router. `ServerState` is cheap to clone (Arcs).
pub fn router(state: ServerState) -> Router {
    Router::new()
        .route("/ws/session", get(ws_session))
        .route("/api/quota", get(quota_handler))
        .route("/api/doctor", get(doctor_handler))
        .route("/api/config", get(config_handler))
        .route("/api/version", get(version_handler))
        .with_state(state)
}

/// Serve on `addr` until the process is terminated.
pub async fn serve(
    addr: std::net::SocketAddr,
    config: Config,
    quota: QuotaStore,
    timeout: Duration,
    config_path: Option<String>,
) -> anyhow::Result<()> {
    let state = ServerState::from_config(config, quota, timeout).with_config_path(config_path);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "consilium server listening (ws: /ws/session)");
    serve_on(listener, state).await
}

/// Serve on a pre-bound listener. The desktop app binds `127.0.0.1:0` itself
/// (to learn the assigned port from `local_addr()`) and passes the listener in.
pub async fn serve_on(listener: tokio::net::TcpListener, state: ServerState) -> anyhow::Result<()> {
    axum::serve(listener, router(state)).await?;
    Ok(())
}

/// True if a WebSocket upgrade with this `Origin` header value is allowed.
/// Absent Origin (non-browser clients) is allowed; a present Origin is allowed
/// only when its host is loopback (localhost / 127.0.0.1 / ::1). This blocks a
/// malicious web page from driving the local server cross-origin (CSRF / DNS-rebind).
fn origin_allowed(origin: Option<&str>) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    // origin looks like "http://localhost:5173" or "https://evil.com"
    let host = origin
        .split("://")
        .nth(1)
        .unwrap_or(origin)
        .split('/')
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    // strip port
    let host_no_port = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    // `tauri.localhost` is the Tauri webview origin host on Linux/Windows
    // (macOS uses `tauri://localhost`, whose host is plain `localhost`).
    matches!(
        host_no_port,
        "localhost" | "127.0.0.1" | "::1" | "[::1]" | "tauri.localhost"
    )
}

async fn ws_session(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<ServerState>,
) -> Response {
    let origin = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    if !origin_allowed(origin) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "cross-origin WebSocket rejected",
        )
            .into_response();
    }
    ws.on_upgrade(move |socket| handle_session(socket, state))
}

/// `GET /api/quota` → current per-provider usage over the rolling window.
async fn quota_handler(State(state): State<ServerState>) -> Json<QuotaSnapshot> {
    Json(quota_snapshot(state.quota.as_ref()))
}

/// `GET /api/doctor` → live auth/liveness probe of every provider. Spawns real
/// CLIs (seconds, ~1 token each) — fetched on demand, never polled.
async fn doctor_handler(State(state): State<ServerState>) -> Json<DoctorReport> {
    let rows = crate::auth::auth_report(state.quota.as_ref()).await;
    Json(DoctorReport {
        providers: rows.iter().map(|(p, a)| provider_status(*p, a)).collect(),
    })
}

/// Map a probe result to its wire shape. Pure — unit-tested below.
fn provider_status(p: Provider, auth: &crate::auth::ProviderAuth) -> ProviderStatus {
    use crate::auth::ProviderAuth;
    let (state, detail) = match auth {
        ProviderAuth::Ready => (AuthState::Ready, String::new()),
        ProviderAuth::NeedsLogin(d) => (AuthState::NeedsLogin, d.clone()),
        ProviderAuth::CliMissing => (AuthState::CliMissing, String::new()),
        ProviderAuth::Down(d) => (AuthState::Down, d.clone()),
    };
    ProviderStatus {
        provider: p,
        state,
        detail,
        hint: crate::auth::guidance(p, auth),
    }
}

/// `GET /api/config` → read-only council summary.
async fn config_handler(State(state): State<ServerState>) -> Json<ConfigSummary> {
    Json((*state.config_summary).clone())
}

/// `GET /api/version` → the server's crate version.
async fn version_handler() -> Json<VersionInfo> {
    Json(VersionInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

/// Build a usage snapshot over the rolling window. Extracted from the handler so
/// it's unit-testable without standing up the HTTP server.
pub fn quota_snapshot(quota: &QuotaStore) -> QuotaSnapshot {
    let since = crate::quota::unix_now() - crate::quota::WINDOW_SECS;
    let usage = |provider: Provider| -> ProviderUsage {
        let (input_tokens, output_tokens) = quota.totals_since(provider, since).unwrap_or((0, 0));
        let (est_in, est_out) = quota
            .estimated_totals_since(provider, since)
            .unwrap_or((0, 0));
        ProviderUsage {
            input_tokens,
            output_tokens,
            estimated: est_in + est_out > 0,
        }
    };
    QuotaSnapshot {
        window_secs: crate::quota::WINDOW_SECS,
        claude: usage(Provider::Claude),
        codex: usage(Provider::Codex),
        gemini: usage(Provider::Gemini),
        grok: usage(Provider::Grok),
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
    // `tx` becomes the run's sink only if a run actually spawns; every path
    // that skips the run MUST drop it before awaiting the writer — a live
    // sender keeps `rx.recv()` pending and deadlocks handle_session against
    // its own writer task.
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let term_tx = tx.clone();

    let writer = tokio::spawn(async move {
        while let Some(json) = rx.recv().await {
            if sender.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
        let _ = sender.send(Message::Close(None)).await;
    });

    let send_terminal = |frame: &ServerFrame| {
        let _ = term_tx.send(serde_json::to_string(frame).unwrap_or_default());
    };

    match req {
        SessionRequest::Cancel => {
            drop(tx);
            send_terminal(&ServerFrame::Error {
                error: "nothing to cancel: the first frame must describe a run".into(),
            });
        }
        SessionRequest::Conduct { task, context, cwd } => {
            let root = state.launch_root.as_ref();
            let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| root.to_path_buf());
            if !cwd_within_root(&cwd, root) {
                send_terminal(&ServerFrame::Error {
                    error: "cwd is outside the server's working directory; refusing to run".into(),
                });
                drop(tx);
                drop(term_tx);
                let _ = writer.await;
                return;
            }
            // One run at a time: a second session while a run is active would
            // interleave git diffs in the same launch root.
            let Ok(_permit) = state.active_run.clone().try_acquire_owned() else {
                send_terminal(&ServerFrame::Error {
                    error: "a run is already active on this server".into(),
                });
                drop(tx);
                drop(term_tx);
                let _ = writer.await;
                return;
            };

            // The run executes in its own task so this loop can keep polling
            // the client socket: `{"kind":"cancel"}` or the socket closing
            // aborts the run — dropped futures SIGKILL agent children via
            // kill_on_drop, so a closed browser tab cannot keep burning quota.
            let deps = (state.deps_fn)();
            let quota = state.quota.clone();
            let timeout = state.timeout;
            let sink: Arc<dyn ProgressSink> = Arc::new(WsSink { tx });
            let mut run = tokio::spawn(PROGRESS_SINK.scope(sink, async move {
                let health = ModelHealth::new();
                run_conduct(&task, &context, deps, quota.as_ref(), cwd, timeout, &health).await
            }));

            let outcome = loop {
                tokio::select! {
                    joined = &mut run => break Some(joined),
                    msg = receiver.next() => match msg {
                        Some(Ok(Message::Text(t)))
                            if matches!(
                                serde_json::from_str(t.as_str()),
                                Ok(SessionRequest::Cancel)
                            ) =>
                        {
                            run.abort();
                            let _ = (&mut run).await; // ensure children are killed
                            break None;
                        }
                        // Unknown text frames are ignored (forward compat).
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                            run.abort();
                            let _ = (&mut run).await;
                            break None;
                        }
                        Some(Ok(_)) => {} // ping/pong/binary
                    }
                }
            };

            let frame = match outcome {
                None => ServerFrame::RunCancelled {},
                Some(Ok(Ok(o))) => ServerFrame::from(&o),
                Some(Ok(Err(e))) => ServerFrame::RunError {
                    error: e.to_string(),
                },
                // The spawned run panicked or was aborted out from under us.
                Some(Err(join_err)) => ServerFrame::RunError {
                    error: format!("run task failed: {join_err}"),
                },
            };
            send_terminal(&frame);
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
        max_replans: config.max_replans,
        budget: config.budget_secs.map(std::time::Duration::from_secs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── origin_allowed ────────────────────────────────────────────────────────

    #[test]
    fn origin_allowed_none_permits_non_browser_clients() {
        assert!(origin_allowed(None));
    }

    #[test]
    fn origin_allowed_localhost_port() {
        assert!(origin_allowed(Some("http://localhost:5173")));
    }

    #[test]
    fn origin_allowed_loopback_ipv4_port() {
        assert!(origin_allowed(Some("http://127.0.0.1:7878")));
    }

    #[test]
    fn origin_allowed_loopback_ipv6_bracketed() {
        assert!(origin_allowed(Some("http://[::1]:8080")));
    }

    #[test]
    fn origin_allowed_rejects_external_domain() {
        assert!(!origin_allowed(Some("https://evil.com")));
    }

    #[test]
    fn origin_allowed_rejects_localhost_subdomain() {
        assert!(!origin_allowed(Some("http://localhost.evil.com")));
    }

    #[test]
    fn origin_allowed_rejects_attacker_subdomain_of_localhost() {
        assert!(!origin_allowed(Some("https://localhost.attacker.com:80")));
    }

    #[test]
    fn origin_allowed_tauri_macos_scheme() {
        // macOS Tauri webview origin: host parses as plain `localhost`.
        assert!(origin_allowed(Some("tauri://localhost")));
    }

    #[test]
    fn origin_allowed_tauri_linux_host() {
        // Linux/Windows Tauri webview origin.
        assert!(origin_allowed(Some("http://tauri.localhost")));
    }

    #[test]
    fn origin_allowed_rejects_tauri_localhost_suffix_attack() {
        assert!(!origin_allowed(Some("http://eviltauri.localhost.evil.com")));
        assert!(!origin_allowed(Some("http://x.tauri.localhost.evil.com")));
    }

    #[test]
    fn provider_status_maps_auth_states() {
        use crate::auth::ProviderAuth;
        let s = provider_status(Provider::Claude, &ProviderAuth::Ready);
        assert_eq!(s.state, AuthState::Ready);
        assert!(s.detail.is_empty());
        assert!(s.hint.contains("ready"), "got: {}", s.hint);

        let s = provider_status(
            Provider::Codex,
            &ProviderAuth::NeedsLogin("401 unauthorized".into()),
        );
        assert_eq!(s.state, AuthState::NeedsLogin);
        assert_eq!(s.detail, "401 unauthorized");
        assert!(s.hint.contains("codex login"), "got: {}", s.hint);

        let s = provider_status(Provider::Gemini, &ProviderAuth::CliMissing);
        assert_eq!(s.state, AuthState::CliMissing);
        assert!(s.hint.contains("install"), "got: {}", s.hint);

        let s = provider_status(Provider::Claude, &ProviderAuth::Down("rate limit".into()));
        assert_eq!(s.state, AuthState::Down);
        assert_eq!(s.detail, "rate limit");
    }

    // NOTE: cwd_within_root unit tests live in crate::confine (the helper is
    // shared with the MCP server).

    // ── existing tests ────────────────────────────────────────────────────────

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
        assert_eq!(snap.grok.input_tokens, 0);
        assert_eq!(snap.grok.output_tokens, 0);
        // Measured-only providers are not flagged estimated.
        assert!(!snap.gemini.estimated && !snap.codex.estimated);

        // An estimated row (agy/Gemini) flips the flag.
        quota.record_estimated(Provider::Gemini, 5, 1).unwrap();
        let snap2 = quota_snapshot(&quota);
        assert!(snap2.gemini.estimated, "gemini now has an estimated row");
        assert!(!snap2.codex.estimated, "codex stays measured-only");
    }
}
