//! A provider CLI must leave nothing running behind it. Zero quota: the
//! "provider" here is a shell script that starts background helpers the way a
//! real CLI starts MCP servers.
//!
//! Regression (2026-11-17): Grok CLI 0.2.87 reads Claude Code's plugins, so it
//! started the Telegram plugin's MCP server — in a process group of its own —
//! and exited without stopping it. That server then ignored SIGTERM and spun a
//! CPU core. Nine of them piled up over an afternoon of `consilium auth` and
//! `consilium models` probes. A helper is modelled here by a loop that ignores
//! SIGTERM and appends a byte to a "heartbeat" file every 50 ms; a test passes
//! only if the heartbeat stops.
//!
//! Needs `sh` and `python3` on PATH.
#![cfg(unix)]

use consilium::adapters::{Adapter, RunRequest};
use consilium::event::{AgentEvent, Provider};
use consilium::orchestrator::runner::{run_to_completion, RunStatus};
use consilium::quota::QuotaStore;
use consilium::sessions;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A helper in the provider's own process group: ignores SIGTERM, beats, and
/// keeps its stdio away from the provider's pipes (as an MCP server would).
const HELPER_IN_GROUP: &str = r#"( trap '' TERM; while :; do printf x >> "$BEAT"; sleep 0.05; done ) </dev/null >/dev/null 2>&1 &"#;

/// The Grok case: a helper that moves to a process group of its own, so
/// signalling the provider's group cannot reach it. It is found by the session
/// tag in its environment instead. (A real MCP server is a `bun`, `node` or
/// `python` process; `python3` stands in for one here. Not a system binary
/// such as `/usr/bin/perl`: macOS hides those processes' environment.)
const HELPER_OWN_GROUP: &str = r#"python3 -c '
import os, signal, time
os.setpgrp()
signal.signal(signal.SIGTERM, signal.SIG_IGN)
while True:
    with open(os.environ["BEAT"], "a") as beat:
        beat.write("x")
    time.sleep(0.05)
' </dev/null >/dev/null 2>&1 &"#;

/// Starts both helpers, gives them time to beat, then answers and exits.
fn finishing_script(helpers: &[&str]) -> String {
    format!("{}\nsleep 0.4\necho done\n", helpers.join("\n"))
}

/// Starts both helpers and then never answers.
fn hanging_script(helpers: &[&str]) -> String {
    // An unusual duration, so a stray one is recognisable in a process list.
    format!("{}\nsleep 611\n", helpers.join("\n"))
}

struct FakeProvider {
    script: String,
    beat: PathBuf,
}

impl Adapter for FakeProvider {
    fn provider(&self) -> Provider {
        Provider::Claude
    }
    fn cli_binary(&self) -> &'static str {
        "sh"
    }
    fn build_command(&self, _req: &RunRequest) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(&self.script).env("BEAT", &self.beat);
        cmd
    }
    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        if line == "done" {
            vec![AgentEvent::Completed {
                result: Some("ok".into()),
            }]
        } else {
            Vec::new()
        }
    }
}

fn request() -> RunRequest {
    RunRequest {
        prompt: "hi".into(),
        model: None,
        cwd: std::env::temp_dir(),
        advisory: false,
        write: false,
    }
}

fn beats(beat: &Path) -> u64 {
    std::fs::metadata(beat).map(|m| m.len()).unwrap_or(0)
}

/// Waits until the helpers have demonstrably started, so a later "it is
/// silent" cannot be true merely because nothing ever ran.
async fn wait_for_beats(beat: &Path, at_least: u64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while beats(beat) < at_least {
        assert!(
            Instant::now() < deadline,
            "the helpers never started beating ({} beats in {})",
            beats(beat),
            beat.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The heartbeat has stopped: no helper is alive.
async fn assert_silent(beat: &Path) {
    // Let a write that was already under way land.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = beats(beat);
    tokio::time::sleep(Duration::from_millis(600)).await;
    let after = beats(beat);
    assert!(before > 0, "the helpers never ran, so this proves nothing");
    assert_eq!(
        after,
        before,
        "a helper outlived its run: {} more beats in 600 ms",
        after - before
    );
}

fn provider(script: String) -> (tempfile::TempDir, PathBuf, Arc<dyn Adapter>) {
    let dir = tempfile::tempdir().unwrap();
    let beat = dir.path().join("beat");
    let adapter = Arc::new(FakeProvider {
        script,
        beat: beat.clone(),
    });
    (dir, beat, adapter)
}

async fn run(adapter: Arc<dyn Adapter>, timeout: Duration) -> RunStatus {
    let quota = QuotaStore::open_in_memory().unwrap();
    run_to_completion(adapter, request(), &quota, timeout)
        .await
        .unwrap()
        .status
}

#[tokio::test]
async fn a_finished_run_leaves_no_helper_in_its_process_group() {
    let (_dir, beat, adapter) = provider(finishing_script(&[HELPER_IN_GROUP]));
    let status = run(adapter, Duration::from_secs(30)).await;
    assert_eq!(status, RunStatus::Completed);
    assert_silent(&beat).await;
}

#[tokio::test]
async fn a_finished_run_leaves_no_helper_that_left_its_process_group() {
    let (_dir, beat, adapter) = provider(finishing_script(&[HELPER_OWN_GROUP]));
    let status = run(adapter, Duration::from_secs(30)).await;
    assert_eq!(status, RunStatus::Completed);
    assert_silent(&beat).await;
}

#[tokio::test]
async fn a_timed_out_run_leaves_no_helpers_behind() {
    let (_dir, beat, adapter) = provider(hanging_script(&[HELPER_IN_GROUP, HELPER_OWN_GROUP]));
    let status = run(adapter, Duration::from_millis(1500)).await;
    assert_eq!(status, RunStatus::TimedOut);
    assert_silent(&beat).await;
}

#[tokio::test]
async fn a_cancelled_run_leaves_no_helpers_behind() {
    let (_dir, beat, adapter) = provider(hanging_script(&[HELPER_IN_GROUP, HELPER_OWN_GROUP]));
    let handle = sessions::spawn(adapter, request()).unwrap();
    wait_for_beats(&beat, 4).await;
    // What a cancelled run does: the session is dropped mid-flight.
    drop(handle);
    assert_silent(&beat).await;
}

/// Stopping consilium itself (Ctrl-C, `kill`, a closed terminal) must not
/// orphan the provider CLI or anything it started. A fake `claude` on PATH
/// stands in for the real one.
#[tokio::test]
async fn stopping_consilium_takes_its_provider_and_helpers_with_it() {
    for (signal, exit_code) in [("TERM", 143), ("INT", 130), ("HUP", 129)] {
        let dir = tempfile::tempdir().unwrap();
        let beat = dir.path().join("beat");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let fake_claude = bin.join("claude");
        std::fs::write(
            &fake_claude,
            format!(
                "#!/bin/sh\n{}",
                hanging_script(&[HELPER_IN_GROUP, HELPER_OWN_GROUP])
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_claude, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut consilium = std::process::Command::new(env!("CARGO_BIN_EXE_consilium"))
            .args(["run", "--provider", "claude", "say hi"])
            .current_dir(dir.path())
            .env("HOME", dir.path())
            .env("BEAT", &beat)
            // the fake `claude` first, then the real PATH (for sh, python3)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        wait_for_beats(&beat, 4).await;

        let sent = std::process::Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(consilium.id().to_string())
            .status()
            .unwrap();
        assert!(sent.success(), "could not signal consilium");
        let status = tokio::task::spawn_blocking(move || consilium.wait().unwrap())
            .await
            .unwrap();
        assert_eq!(
            status.code(),
            Some(exit_code),
            "SIG{signal}: consilium should clean up and exit with {exit_code}"
        );
        assert_silent(&beat).await;
    }
}
