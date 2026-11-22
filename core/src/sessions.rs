use crate::adapters::{sign_in_failure, Adapter, RunRequest, STDERR_TAIL_LABEL};
use crate::event::AgentEvent;
use crate::reaper::Reaper;
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
    /// left orphaned.
    pub task: ReaderTask,
}

/// Owns the stream-reader `JoinHandle` and aborts it on drop. The reader task
/// owns the child process, so any path that drops a live session — an explicit
/// timeout, a cancelled run whose future is dropped mid-await, a panic unwind —
/// kills the agent child instead of leaving it running detached. Abort on an
/// already-finished task is a no-op, so the normal path is unaffected.
#[derive(Debug)]
pub struct ReaderTask(pub tokio::task::JoinHandle<()>);

impl ReaderTask {
    pub fn abort(&self) {
        self.0.abort();
    }
}

impl Drop for ReaderTask {
    fn drop(&mut self) {
        self.0.abort();
    }
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

/// How far into stderr a sign-in prompt can be. A CLI with no sign-in asks
/// before it says anything else; the same words further on are something a
/// working CLI logged — a worker editing this very code, say.
const SIGN_IN_HEAD_BYTES: usize = 2048;

/// Watches the opening of a CLI's stderr for one of its sign-in prompts.
struct SignInWatch {
    prompts: &'static [&'static str],
    /// The first bytes of stderr, up to `SIGN_IN_HEAD_BYTES`.
    head: Vec<u8>,
    /// Nothing more to look for: a prompt was found, or the head is full.
    done: bool,
}

impl SignInWatch {
    fn new(prompts: &'static [&'static str]) -> Self {
        Self {
            prompts,
            head: Vec::new(),
            done: prompts.is_empty(),
        }
    }

    /// Takes the next bytes read from stderr; returns the prompt they
    /// complete, once.
    fn feed(&mut self, chunk: &[u8]) -> Option<&'static str> {
        if self.done {
            return None;
        }
        let room = SIGN_IN_HEAD_BYTES - self.head.len();
        self.head.extend_from_slice(&chunk[..chunk.len().min(room)]);
        let said = String::from_utf8_lossy(&self.head);
        let found = self
            .prompts
            .iter()
            .copied()
            .find(|prompt| said.contains(prompt));
        self.done = found.is_some() || self.head.len() >= SIGN_IN_HEAD_BYTES;
        found
    }
}

/// [`drain_stderr`]'s result is the tail; a sign-in prompt in the head is
/// reported on `asked_to_sign_in`.
async fn drain_stderr(
    stderr: tokio::process::ChildStderr,
    sign_in_prompts: &'static [&'static str],
    asked_to_sign_in: mpsc::Sender<&'static str>,
) -> String {
    use tokio::io::AsyncReadExt;
    let mut reader = tokio::io::BufReader::new(stderr);
    let mut tail: Vec<u8> = Vec::with_capacity(STDERR_TAIL_BYTES);
    let mut buf = [0u8; 4096];
    let mut watch = SignInWatch::new(sign_in_prompts);
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if let Some(prompt) = watch.feed(&buf[..n]) {
                    let _ = asked_to_sign_in.try_send(prompt);
                }
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

/// The failure message for a non-zero exit: the status plus whatever the CLI
/// explained on stderr.
fn exit_error(status: &str, stderr_tail: &str) -> String {
    let stderr_tail = stderr_tail.trim();
    if stderr_tail.is_empty() {
        format!("process exited with {status}")
    } else {
        format!("process exited with {status}; {STDERR_TAIL_LABEL}{stderr_tail}")
    }
}

// Design note: the double-terminal-event case (a streaming adapter emits a
// terminal event via parsing AND the process exit status adds Failed) is
// deliberately NOT deduplicated — the orchestrator treats the first terminal
// event as authoritative.

/// Spawns the adapter's CLI process and streams normalized events.
/// The channel closes when the process exits and all events are delivered.
pub fn spawn(adapter: Arc<dyn Adapter>, req: RunRequest) -> anyhow::Result<SessionHandle> {
    spawn_with_secrets(adapter, req, crate::secrets::default_path().as_deref())
}

/// [`spawn`] with an explicit secrets file (`None` = hand the CLI nothing).
pub fn spawn_with_secrets(
    adapter: Arc<dyn Adapter>,
    req: RunRequest,
    secrets_file: Option<&std::path::Path>,
) -> anyhow::Result<SessionHandle> {
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
    // A process started by a desktop app has no shell profile, hence none of
    // the credentials a terminal exports: hand the CLI the saved ones it is
    // missing. What the environment already holds always wins.
    if let Some(file) = secrets_file {
        let saved = crate::secrets::missing_credentials(adapter.credential_env(), file, |key| {
            std::env::var_os(key).is_some_and(|value| !value.is_empty())
        });
        cmd.envs(saved);
    }
    // Fence the run: its own process group plus a session tag, so that nothing
    // the CLI starts (an MCP server, say) can outlive it — see `crate::reaper`.
    let mut reaper = Reaper::fence(&mut cmd);
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        // So that aborting the reader task (on timeout/cancel) SIGKILLs the child
        // instead of orphaning it — see SessionHandle::task and run_to_completion.
        .kill_on_drop(true);
    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn {}: {e}", adapter.cli_binary()))?;
    reaper.watch(child.id());
    tracing::info!(session = %id, provider = adapter.provider().as_str(), "session spawned");

    let task = tokio::spawn(run_session(adapter, child, reaper, tx, id.clone()));

    Ok(SessionHandle {
        id,
        events: rx,
        task: ReaderTask(task),
    })
}

/// Runs one spawned CLI to its end, streaming its events to `tx`. Owns the
/// child and its `reaper`: when this task is aborted (timeout, cancel) or
/// returns early, dropping them kills the CLI and everything it started.
async fn run_session(
    adapter: Arc<dyn Adapter>,
    mut child: tokio::process::Child,
    reaper: Reaper,
    tx: mpsc::Sender<AgentEvent>,
    id: String,
) {
    let stdout = child.stdout.take().expect("stdout piped by spawn");
    let stderr = child.stderr.take().expect("stderr piped by spawn");
    let (full_output, stderr_tail) = match read_output(adapter.as_ref(), stdout, stderr, &tx).await
    {
        OutputEnd::Closed {
            stdout,
            stderr_tail,
        } => (stdout, stderr_tail),
        // Returning drops the child, which is SIGKILLed (kill_on_drop). The
        // stderr-drain task then ends when the killed child's stderr closes.
        OutputEnd::ReceiverGone => return,
        OutputEnd::AskedToSignIn(prompt) => {
            tracing::warn!(session = %id, "session stopped: the CLI asked for a sign-in");
            stop_waiting_cli(child, reaper).await;
            let error = sign_in_failure(adapter.cli_binary(), prompt);
            let _ = tx.send(AgentEvent::Failed { error }).await;
            return;
        }
    };
    // The CLI's output is closed, so the process is done (or about to be):
    // settle its exit status BEFORE the adapter's end-of-stream parsing. A
    // non-zero exit is reported first — the first terminal event is
    // authoritative — so the CLI's own stderr explanation is never masked by
    // an adapter's generic end-of-stream fallback ("produced no output"), and
    // a run that died mid-answer is never reported as completed on its
    // partial text. Streaming adapters are unaffected: their terminal event
    // was already sent line by line.
    let exit_failure = match child.wait().await {
        Ok(status) if !status.success() => {
            tracing::warn!(session = %id, %status, "session process exited non-zero");
            Some(exit_error(&status.to_string(), &stderr_tail))
        }
        Err(e) => Some(e.to_string()),
        Ok(_) => None,
    };
    let final_events = exit_failure
        .map(|error| AgentEvent::Failed { error })
        .into_iter()
        .chain(adapter.parse_final(&full_output));
    for ev in final_events {
        if tx.send(ev).await.is_err() {
            // Receiver dropped — nothing left to deliver to.
            return;
        }
    }
    // The CLI has exited. Whatever it left running is asked to stop and
    // killed if it does not; the session ends only once that is done.
    reaper.finish().await;
}

/// How reading a CLI's output ended.
enum OutputEnd {
    /// The CLI closed stdout and stderr: all of the one, the tail of the other.
    Closed { stdout: String, stderr_tail: String },
    /// No one is listening for events any more.
    ReceiverGone,
    /// The CLI stopped to ask — on stderr, in these words — for a sign-in.
    AskedToSignIn(&'static str),
}

/// Reads a CLI's output to its end — stdout as events, line by line, and
/// stderr for its tail — or until the CLI asks on stderr for a sign-in.
async fn read_output(
    adapter: &dyn Adapter,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    tx: &mpsc::Sender<AgentEvent>,
) -> OutputEnd {
    let (asked_tx, mut asked_to_sign_in) = mpsc::channel(1);
    let mut stderr_task = tokio::spawn(drain_stderr(stderr, adapter.sign_in_prompts(), asked_tx));
    let mut full_output = String::new();
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let line = tokio::select! {
            Some(prompt) = asked_to_sign_in.recv() => return OutputEnd::AskedToSignIn(prompt),
            line = lines.next_line() => line,
        };
        let Ok(Some(line)) = line else { break };
        full_output.push_str(&line);
        full_output.push('\n');
        for ev in adapter.parse_line(&line) {
            if tx.send(ev).await.is_err() {
                return OutputEnd::ReceiverGone;
            }
        }
    }
    // stdout is closed, yet the CLI may still be there, waiting for a sign-in:
    // stderr is watched until it closes as well. A prompt outranks the close,
    // so a CLI that asked and then gave up is still reported as having asked.
    tokio::select! {
        biased;
        Some(prompt) = asked_to_sign_in.recv() => OutputEnd::AskedToSignIn(prompt),
        tail = &mut stderr_task => OutputEnd::Closed {
            stdout: full_output,
            stderr_tail: tail.unwrap_or_default(),
        },
    }
}

/// Ends a CLI that is waiting for a sign-in no one is there to give: it and
/// everything it started are killed at once, as for a cancelled run, and the
/// CLI is seen gone before the failure is reported.
async fn stop_waiting_cli(mut child: tokio::process::Child, reaper: Reaper) {
    // Dropping the reaper kills the run's whole process group; that scans the
    // process table, so it is kept off the async threads.
    let _ = tokio::task::spawn_blocking(move || drop(reaper)).await;
    let _ = child.kill().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROMPTS: &[&str] = &["Please sign in:"];

    #[test]
    fn a_prompt_that_opens_stderr_is_found_once() {
        let mut watch = SignInWatch::new(PROMPTS);
        assert_eq!(
            watch.feed(b"Please sign in: https://example.test\n"),
            Some("Please sign in:")
        );
        assert_eq!(watch.feed(b"Please sign in: again\n"), None);
    }

    #[test]
    fn a_prompt_split_across_two_reads_is_found() {
        let mut watch = SignInWatch::new(PROMPTS);
        assert_eq!(watch.feed(b"\nPlease si"), None);
        assert_eq!(watch.feed(b"gn in: https://"), Some("Please sign in:"));
    }

    /// However the reads fall, the words are a prompt only in the head.
    #[test]
    fn a_prompt_past_the_head_is_never_found() {
        let mut watch = SignInWatch::new(PROMPTS);
        assert_eq!(watch.feed(&[b'.'; SIGN_IN_HEAD_BYTES]), None);
        assert_eq!(watch.feed(b"Please sign in: quoted from a file\n"), None);
        for _ in 0..8 {
            assert_eq!(watch.feed(&[b'.'; 1500]), None);
        }
    }

    /// A prompt cut in two by the end of the head is not one either: the
    /// second half lies outside it.
    #[test]
    fn a_prompt_that_straddles_the_end_of_the_head_is_not_found() {
        let mut watch = SignInWatch::new(PROMPTS);
        assert_eq!(watch.feed(&[b'.'; SIGN_IN_HEAD_BYTES - 5]), None);
        assert_eq!(watch.feed(b"Please sign in: quoted"), None);
    }

    #[test]
    fn a_cli_that_names_no_prompt_is_not_watched() {
        let mut watch = SignInWatch::new(&[]);
        assert_eq!(watch.feed(b"Please sign in: https://example.test\n"), None);
    }
}
