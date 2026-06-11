use crate::adapters::{Adapter, RunRequest};
use crate::event::AgentEvent;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

#[derive(Debug)]
pub struct SessionHandle {
    pub id: String,
    pub events: mpsc::Receiver<AgentEvent>,
}

fn next_session_id(provider: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before unix epoch")
        .as_nanos();
    format!("{provider}-{nanos}")
}

// Design note: the double-terminal-event case (adapter emits Completed via
// parsing AND process exit status adds Failed) is deliberately NOT deduplicated
// in M1 — the orchestrator (M2) treats the first terminal event as authoritative.

/// Spawns the adapter's CLI process and streams normalized events.
/// The channel closes when the process exits and all events are delivered.
pub fn spawn(adapter: Arc<dyn Adapter>, req: RunRequest) -> anyhow::Result<SessionHandle> {
    let (tx, rx) = mpsc::channel::<AgentEvent>(256);
    let id = next_session_id(adapter.provider().as_str());

    let mut cmd = adapter.build_command(&req);
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {}: {e}", adapter.cli_binary()))?;
    let stdout = child.stdout.take().expect("stdout piped above");

    tokio::spawn(async move {
        let mut full_output = String::new();
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            full_output.push_str(&line);
            full_output.push('\n');
            for ev in adapter.parse_line(&line) {
                if tx.send(ev).await.is_err() {
                    return; // receiver dropped — stop streaming
                }
            }
        }
        for ev in adapter.parse_final(&full_output) {
            if tx.send(ev).await.is_err() {
                return;
            }
        }
        match child.wait().await {
            Ok(status) if !status.success() => {
                let _ = tx
                    .send(AgentEvent::Failed {
                        error: format!("process exited with {status}"),
                    })
                    .await;
            }
            Err(e) => {
                let _ = tx
                    .send(AgentEvent::Failed {
                        error: e.to_string(),
                    })
                    .await;
            }
            _ => {}
        }
    });

    Ok(SessionHandle { id, events: rx })
}
