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
    /// The stream-reader task. Aborting it drops the child process — spawned with
    /// `kill_on_drop(true)` — so a timed-out or cancelled run is SIGKILLed, not
    /// left orphaned. On the normal path the JoinHandle is simply detached.
    pub task: tokio::task::JoinHandle<()>,
}

fn next_session_id(provider: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before unix epoch")
        .as_nanos();
    format!("{provider}-{nanos}")
}

/// Drains stderr concurrently (prevents pipe-buffer deadlock) keeping only
/// the last `STDERR_TAIL_BYTES` for diagnostics.
const STDERR_TAIL_BYTES: usize = 4096;

async fn drain_stderr(stderr: tokio::process::ChildStderr) -> String {
    use tokio::io::AsyncReadExt;
    let mut reader = tokio::io::BufReader::new(stderr);
    let mut tail: Vec<u8> = Vec::with_capacity(STDERR_TAIL_BYTES);
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                tail.extend_from_slice(&buf[..n]);
                if tail.len() > STDERR_TAIL_BYTES {
                    let excess = tail.len() - STDERR_TAIL_BYTES;
                    tail.drain(..excess);
                }
            }
        }
    }
    String::from_utf8_lossy(&tail).into_owned()
}

// Design note: the double-terminal-event case (adapter emits Completed via
// parsing AND process exit status adds Failed) is deliberately NOT deduplicated
// in M1 — the orchestrator (M2) treats the first terminal event as authoritative.

/// Spawns the adapter's CLI process and streams normalized events.
/// The channel closes when the process exits and all events are delivered.
pub fn spawn(adapter: Arc<dyn Adapter>, req: RunRequest) -> anyhow::Result<SessionHandle> {
    // INVARIANT: advisory and write must never both be true — advisory relaxes
    // workspace-trust safeguards (codex: --skip-git-repo-check) while write
    // auto-approves edits; combined they yield the most permissive and least
    // protected run possible. Deliberation never writes. A hard bail (not a
    // debug_assert) so the guard is real in --release too — it is a no-op in
    // practice (the only write:true caller hard-codes advisory:false) but the
    // codebase advertises this as load-bearing, so it must hold in every build.
    if req.advisory && req.write {
        anyhow::bail!(
            "advisory + write is a contradictory RunRequest: deliberation runs never mutate files"
        );
    }
    // Bounded channel: sender stalls when consumer is 256 events behind
    // (cooperative backpressure — the child's stdout pipe fills next).
    let (tx, rx) = mpsc::channel::<AgentEvent>(256);
    let id = next_session_id(adapter.provider().as_str());

    let mut cmd = adapter.build_command(&req);
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        // So that aborting the reader task (on timeout/cancel) SIGKILLs the child
        // instead of orphaning it — see SessionHandle::task and run_to_completion.
        .kill_on_drop(true);
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {}: {e}", adapter.cli_binary()))?;
    let stdout = child.stdout.take().expect("stdout piped above");
    let stderr = child.stderr.take().expect("stderr piped above");
    tracing::info!(session = %id, provider = adapter.provider().as_str(), "session spawned");

    let task_id = id.clone();
    let task = tokio::spawn(async move {
        let stderr_task = tokio::spawn(drain_stderr(stderr));
        let mut full_output = String::new();
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            full_output.push_str(&line);
            full_output.push('\n');
            for ev in adapter.parse_line(&line) {
                if tx.send(ev).await.is_err() {
                    // Receiver dropped → returning drops the child, which is
                    // SIGKILLed (kill_on_drop). The stderr-drain task then ends
                    // when the killed child's stderr closes.
                    return;
                }
            }
        }
        for ev in adapter.parse_final(&full_output) {
            if tx.send(ev).await.is_err() {
                // Receiver dropped → returning drops the child, which is
                // SIGKILLed (kill_on_drop). The stderr-drain task then ends
                // when the killed child's stderr closes.
                return;
            }
        }
        let stderr_tail = stderr_task.await.unwrap_or_default();
        match child.wait().await {
            Ok(status) if !status.success() => {
                tracing::warn!(session = %task_id, %status, "session process exited non-zero");
                let mut error = format!("process exited with {status}");
                if !stderr_tail.trim().is_empty() {
                    error.push_str(&format!("; stderr tail: {}", stderr_tail.trim()));
                }
                let _ = tx.send(AgentEvent::Failed { error }).await;
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

    Ok(SessionHandle {
        id,
        events: rx,
        task,
    })
}
