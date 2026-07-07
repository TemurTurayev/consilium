//! Tauri commands invoked from the UI (`window.__TAURI__.core.invoke`).

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::desktop_config::{desktop_config_path, save, DesktopConfig};
use crate::server_lifecycle::{start_server, DesktopState};

/// Shape returned to the UI by both `get_server_state` and `pick_workspace`.
/// `camelCase` to match `ui/src/runtime.ts`'s `TauriServerState`.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStateDto {
    pub server_url: Option<String>,
    pub workspace: Option<String>,
    pub error: Option<String>,
}

/// Snapshot the current server state without touching it.
#[tauri::command]
pub fn get_server_state(state: State<'_, Arc<DesktopState>>) -> ServerStateDto {
    let guard = state.server.lock().expect("server mutex poisoned");
    match guard.as_ref() {
        Some(running) => {
            let error = running.error.lock().expect("error mutex poisoned").clone();
            ServerStateDto {
                // A running task that has recorded an error is no longer
                // serving; don't hand the UI a dead URL.
                server_url: if error.is_some() {
                    None
                } else {
                    Some(running.url.clone())
                },
                workspace: Some(running.workspace.to_string_lossy().to_string()),
                error,
            }
        }
        None => ServerStateDto::default(),
    }
}

/// Open a native folder picker, persist the choice, and (re)start the
/// server against it. Returns the same shape as `get_server_state`.
#[tauri::command]
pub async fn pick_workspace(
    app: AppHandle,
    state: State<'_, Arc<DesktopState>>,
) -> Result<ServerStateDto, String> {
    let picked = pick_folder(&app).await;
    let Some(workspace) = picked else {
        // User cancelled the dialog: report current state unchanged.
        return Ok(get_server_state(state));
    };

    let desktop_config = DesktopConfig {
        workspace: Some(workspace.to_string_lossy().to_string()),
    };
    if let Err(err) = save(&desktop_config_path(), &desktop_config) {
        tracing::warn!(error = %err, "failed to persist desktop.json");
    }

    let state = state.inner().clone();
    match start_server(&state, workspace.clone()).await {
        Ok(url) => Ok(ServerStateDto {
            server_url: Some(url),
            workspace: Some(workspace.to_string_lossy().to_string()),
            error: None,
        }),
        Err(err) => Ok(ServerStateDto {
            server_url: None,
            workspace: Some(workspace.to_string_lossy().to_string()),
            error: Some(err.to_string()),
        }),
    }
}

/// Runs the (blocking) native folder dialog on a blocking thread and awaits
/// the result, so the async command doesn't stall the Tokio runtime.
async fn pick_folder(app: &AppHandle) -> Option<PathBuf> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |folder| {
        let path = folder.and_then(|f| f.into_path().ok());
        let _ = tx.send(path);
    });
    rx.await.ok().flatten()
}
