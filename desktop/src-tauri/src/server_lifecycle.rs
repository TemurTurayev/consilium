//! Owns the in-process consilium server: starting it against a chosen
//! workspace, tearing down the previous run on restart, and reporting state
//! back to the UI via `get_server_state`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use consilium::config::Config;
use consilium::quota::QuotaStore;
use consilium::server::{serve_on, ServerState};
use tokio::net::TcpListener;
use tokio::task::AbortHandle;

use crate::desktop_config::usage_db_path;

/// Timeout passed to `ServerState` for a single conduct run.
const SERVE_TIMEOUT_SECS: u64 = 900;

/// A currently (or formerly) running server task.
pub struct RunningServer {
    pub url: String,
    pub workspace: PathBuf,
    abort: AbortHandle,
    /// Populated once the spawned `serve_on` future finishes with an error
    /// (e.g. the listener died). Surfaced to the UI by `get_server_state`.
    pub error: Mutex<Option<String>>,
}

/// Tauri-managed state: the currently running server, if any. Wrapped in
/// `Arc` at the call site (not here) so the background watcher task can hold
/// a handle without borrowing from `tauri::State`.
#[derive(Default)]
pub struct DesktopState {
    pub server: Mutex<Option<Arc<RunningServer>>>,
}

/// Resolve which config file to load for a workspace: workspace-local
/// `consilium.config.json` if present, else `~/.consilium/consilium.config.json`,
/// else defaults (via `Config::load(None)`).
fn resolve_config_path(workspace: &Path) -> Option<PathBuf> {
    let workspace_config = workspace.join("consilium.config.json");
    if workspace_config.is_file() {
        return Some(workspace_config);
    }
    let home_config = dirs::home_dir().map(|h| h.join(".consilium").join("consilium.config.json"));
    if let Some(path) = &home_config {
        if path.is_file() {
            return Some(path.clone());
        }
    }
    None
}

/// Start (or restart) the embedded server against `workspace`. Aborts any
/// previously running server task first. Returns the bound `http://…` URL on
/// success.
pub async fn start_server(state: &Arc<DesktopState>, workspace: PathBuf) -> anyhow::Result<String> {
    // Abort any previous run before starting a new one so restarts never
    // leave two servers bound at once.
    {
        let guard = state.server.lock().expect("server mutex poisoned");
        if let Some(running) = guard.as_ref() {
            running.abort.abort();
        }
    }

    let config_path = resolve_config_path(&workspace);
    let config_path_str = config_path
        .as_ref()
        .map(|p| p.to_string_lossy().to_string());
    let config = Config::load(config_path.as_deref())?;

    let quota = QuotaStore::open(&usage_db_path())?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let local_addr = listener.local_addr()?;
    let url = format!("http://{local_addr}");

    let server_state =
        ServerState::from_config(config, quota, Duration::from_secs(SERVE_TIMEOUT_SECS))
            .with_launch_root(workspace.clone())
            .with_config_path(config_path_str);

    // Plain `tokio::task::spawn` (not `tauri::async_runtime::spawn`): we're
    // already running inside Tauri's Tokio runtime, and using tokio's task
    // handle directly gives us `AbortHandle` + `JoinError::is_cancelled`
    // without going through `tauri::async_runtime`'s thinner wrapper type.
    let url_for_task = url.clone();
    let task = tokio::task::spawn(async move {
        if let Err(err) = serve_on(listener, server_state).await {
            tracing::error!(url = %url_for_task, error = %err, "consilium server exited with error");
            Some(err.to_string())
        } else {
            None
        }
    });

    let running = Arc::new(RunningServer {
        url: url.clone(),
        workspace,
        abort: task.abort_handle(),
        error: Mutex::new(None),
    });

    *state.server.lock().expect("server mutex poisoned") = Some(running.clone());

    // Watch the task in the background: if it finishes (normally, with an
    // error, or via panic), record the error on this specific `RunningServer`
    // slot so a later `get_server_state` call can surface it. Holding `running`
    // (not the outer `DesktopState`) means this only ever touches the slot it
    // created, even if the user has since restarted the server again.
    tokio::task::spawn(async move {
        let error = match task.await {
            Ok(Some(err)) => Some(err),
            Ok(None) => None,
            Err(join_err) if join_err.is_cancelled() => None,
            Err(join_err) => Some(join_err.to_string()),
        };
        if let Some(error) = error {
            *running.error.lock().expect("error mutex poisoned") = Some(error);
        }
    });

    Ok(url)
}
