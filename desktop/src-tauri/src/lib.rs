mod commands;
mod desktop_config;
mod server_lifecycle;

use std::path::PathBuf;
use std::sync::Arc;

use desktop_config::{desktop_config_path, load, save, DesktopConfig};
use server_lifecycle::{start_server, DesktopState};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(DesktopState::default()))
        .invoke_handler(tauri::generate_handler![
            commands::get_server_state,
            commands::pick_workspace,
        ])
        .setup(|app| {
            let state = app.state::<Arc<DesktopState>>().inner().clone();
            tauri::async_runtime::spawn(async move {
                auto_start_saved_workspace(state).await;
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running consilium desktop app");
}

/// On launch, read `~/.consilium/desktop.json`. If it names a workspace that
/// still exists as a directory, start the server against it immediately; if
/// the workspace is gone, clear the stale entry so the UI falls back to the
/// workspace-picker empty state instead of showing a dead path forever.
async fn auto_start_saved_workspace(state: Arc<DesktopState>) {
    let config_path = desktop_config_path();
    let desktop_config = load(&config_path);
    let Some(workspace) = desktop_config.workspace.as_ref().map(PathBuf::from) else {
        return;
    };

    if !workspace.is_dir() {
        tracing::warn!(path = %workspace.display(), "saved workspace no longer exists; clearing");
        let cleared = DesktopConfig { workspace: None };
        if let Err(err) = save(&config_path, &cleared) {
            tracing::warn!(error = %err, "failed to clear stale desktop.json");
        }
        return;
    }

    if let Err(err) = start_server(&state, workspace).await {
        tracing::error!(error = %err, "failed to auto-start server for saved workspace");
    }
}
