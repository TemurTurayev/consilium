//! Making sure a provider CLI leaves nothing running behind it.
//!
//! Provider CLIs start helpers of their own — MCP servers above all — and do
//! not always stop them. Found the hard way (2026-11-17): Grok CLI 0.2.87 reads
//! Claude Code's plugins, started the Telegram plugin's MCP server in a process
//! group of its own and exited without stopping it; that server then ignored
//! SIGTERM and spun a CPU core. Every `consilium auth` left one more behind.
//!
//! Whose bug it is does not matter to the person whose laptop is overheating,
//! so every provider run is fenced in two ways:
//!
//! 1. **Process group.** The CLI is started as the leader of a new process
//!    group, so it and its ordinary helpers can be signalled together.
//! 2. **Session tag.** The CLI is started with a unique environment variable.
//!    A helper that moves to another process group (the Grok case) still
//!    inherits the environment, so it can be found by the tag.
//!
//! When a run ends on its own, whatever is left is asked to stop (SIGTERM) and
//! killed if it has not within a short grace period. When a run is cancelled,
//! times out, or consilium itself is stopped, everything is killed at once.
//!
//! Because provider CLIs no longer share consilium's process group, a Ctrl-C in
//! the terminal reaches only consilium — so the binary must call [`kill_all`]
//! from its signal handlers (see `main.rs`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The environment variable that marks every process descended from one
/// provider run.
pub const SESSION_TAG_VAR: &str = "CONSILIUM_SESSION_TAG";

/// How long leftovers get to exit after SIGTERM before they are killed.
const GRACE: Duration = Duration::from_millis(1500);
const POLL: Duration = Duration::from_millis(100);

/// Runs in flight: session tag → process group. What [`kill_all`] sweeps.
fn live_runs() -> &'static Mutex<HashMap<String, Option<i32>>> {
    static LIVE: OnceLock<Mutex<HashMap<String, Option<i32>>>> = OnceLock::new();
    LIVE.get_or_init(Mutex::default)
}

/// A tag no other run — in this process or another consilium — will have.
fn new_tag() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{}-{nanos}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The fence around one provider run. Dropping it without calling
/// [`Reaper::finish`] — a cancelled or timed-out run — kills everything the
/// run started.
#[derive(Debug)]
pub struct Reaper {
    tag: String,
    group: Option<i32>,
    finished: bool,
}

impl Reaper {
    /// Fence a command that is about to be spawned: tag it and make it the
    /// leader of a new process group.
    pub fn fence(cmd: &mut tokio::process::Command) -> Self {
        let tag = new_tag();
        cmd.env(SESSION_TAG_VAR, &tag);
        #[cfg(unix)]
        cmd.process_group(0);
        live_runs().lock().unwrap().insert(tag.clone(), None);
        Self {
            tag,
            group: None,
            finished: false,
        }
    }

    /// Record the spawned child: as its group's leader, its pid is the group id.
    pub fn watch(&mut self, child_pid: Option<u32>) {
        self.group = child_pid.and_then(|pid| i32::try_from(pid).ok());
        live_runs()
            .lock()
            .unwrap()
            .insert(self.tag.clone(), self.group);
    }

    /// The run ended on its own: ask whatever it left behind to stop, and kill
    /// what has not stopped within the grace period. Costs one process-table
    /// scan when nothing is left.
    pub async fn finish(mut self) {
        self.finished = true;
        live_runs().lock().unwrap().remove(&self.tag);
        let (tag, group) = (self.tag.clone(), self.group);
        // The scan runs `ps` and the wait sleeps: keep both off the async threads.
        let _ = tokio::task::spawn_blocking(move || stop_gracefully(&tag, group)).await;
    }
}

impl Drop for Reaper {
    fn drop(&mut self) {
        if !self.finished {
            live_runs().lock().unwrap().remove(&self.tag);
            kill_now(&self.tag, self.group);
        }
    }
}

/// Kill every provider run still in flight, and everything it started. For
/// consilium's own exit paths (signal handlers): destructors do not run when a
/// process is stopped by a signal, and the provider CLIs are not in consilium's
/// process group, so nothing else would stop them.
pub fn kill_all() {
    let runs: Vec<(String, Option<i32>)> = live_runs().lock().unwrap().drain().collect();
    for (tag, group) in runs {
        kill_now(&tag, group);
    }
}

fn stop_gracefully(tag: &str, group: Option<i32>) {
    if !platform::signal_everything(tag, group, platform::Signal::Term) {
        return;
    }
    let deadline = Instant::now() + GRACE;
    while Instant::now() < deadline {
        std::thread::sleep(POLL);
        if !platform::anything_left(tag, group) {
            return;
        }
    }
    platform::signal_everything(tag, group, platform::Signal::Kill);
}

fn kill_now(tag: &str, group: Option<i32>) {
    platform::signal_everything(tag, group, platform::Signal::Kill);
}

/// Pids in `ps axeww -o pid=,command=` output whose environment holds
/// `needle` (`VAR=value`) as a whole word. Pure.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn pids_carrying(ps_output: &str, needle: &str) -> Vec<i32> {
    ps_output
        .lines()
        .filter(|line| {
            line.match_indices(needle).any(|(at, _)| {
                let before = line[..at].chars().next_back();
                let after = line[at + needle.len()..].chars().next();
                before.is_none_or(char::is_whitespace) && after.is_none_or(char::is_whitespace)
            })
        })
        .filter_map(|line| line.split_whitespace().next()?.parse().ok())
        .collect()
}

#[cfg(unix)]
mod platform {
    use super::SESSION_TAG_VAR;
    use rustix::process::{
        kill_process, kill_process_group, test_kill_process_group, Pid, Signal as RawSignal,
    };

    #[derive(Clone, Copy)]
    pub enum Signal {
        Term,
        Kill,
    }

    impl Signal {
        fn raw(self) -> RawSignal {
            match self {
                Self::Term => RawSignal::TERM,
                Self::Kill => RawSignal::KILL,
            }
        }
    }

    fn group_pid(group: Option<i32>) -> Option<Pid> {
        group.and_then(Pid::from_raw)
    }

    /// A process group id stays reserved while the group has any member, so
    /// signalling it after its leader has exited reaches only the leader's
    /// leftovers.
    fn group_alive(group: Option<i32>) -> bool {
        group_pid(group).is_some_and(|pid| test_kill_process_group(pid).is_ok())
    }

    /// Signal the run's process group and every process carrying its tag.
    /// Returns whether there was anything to signal.
    pub fn signal_everything(tag: &str, group: Option<i32>, signal: Signal) -> bool {
        let group_was_alive = group_alive(group);
        if let Some(pid) = group_pid(group).filter(|_| group_was_alive) {
            let _ = kill_process_group(pid, signal.raw());
        }
        let tagged = tagged_pids(tag);
        for pid in tagged.iter().copied().filter_map(Pid::from_raw) {
            let _ = kill_process(pid, signal.raw());
        }
        group_was_alive || !tagged.is_empty()
    }

    pub fn anything_left(tag: &str, group: Option<i32>) -> bool {
        group_alive(group) || !tagged_pids(tag).is_empty()
    }

    /// Every process — other than this one — whose environment carries the tag.
    fn tagged_pids(tag: &str) -> Vec<i32> {
        let needle = format!("{SESSION_TAG_VAR}={tag}");
        let own = std::process::id() as i32;
        scan(&needle)
            .into_iter()
            .filter(|pid| *pid != own)
            .collect()
    }

    /// Linux: every process's environment is a file.
    #[cfg(target_os = "linux")]
    fn scan(needle: &str) -> Vec<i32> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse::<i32>().ok())
            .filter(|pid| {
                std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|environ| {
                    environ
                        .split(|byte| *byte == 0)
                        .any(|var| var == needle.as_bytes())
                })
            })
            .collect()
    }

    /// macOS and the BSDs: `ps` prints the environment of the user's own
    /// processes after the command line.
    #[cfg(not(target_os = "linux"))]
    fn scan(needle: &str) -> Vec<i32> {
        let Ok(output) = std::process::Command::new("ps")
            .args(["axeww", "-o", "pid=,command="])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            return Vec::new();
        };
        super::pids_carrying(&String::from_utf8_lossy(&output.stdout), needle)
    }
}

/// Without process groups or a way to read another process's environment
/// there is nothing to sweep; consilium does not support such platforms.
#[cfg(not(unix))]
mod platform {
    #[derive(Clone, Copy)]
    pub enum Signal {
        Term,
        Kill,
    }

    pub fn signal_everything(_tag: &str, _group: Option<i32>, _signal: Signal) -> bool {
        false
    }

    pub fn anything_left(_tag: &str, _group: Option<i32>) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_unique_within_a_process() {
        let tags: std::collections::HashSet<String> = (0..100).map(|_| new_tag()).collect();
        assert_eq!(tags.len(), 100);
    }

    #[test]
    fn finds_the_processes_that_carry_the_tag() {
        let ps = "\
  101 /bin/sh /tmp/claude -p hi HOME=/Users/a CONSILIUM_SESSION_TAG=7-1-0 PATH=/bin
  102 bun server.ts PATH=/bin CONSILIUM_SESSION_TAG=7-1-0
  103 bun server.ts PATH=/bin CONSILIUM_SESSION_TAG=7-1-01
  104 grep CONSILIUM_SESSION_TAG=7-1-0x PATH=/bin
  105 vim notes.txt PATH=/bin
";
        // 103 and 104 carry a different tag that merely starts the same way
        assert_eq!(
            pids_carrying(ps, "CONSILIUM_SESSION_TAG=7-1-0"),
            vec![101, 102]
        );
    }

    #[test]
    fn a_tag_inside_another_word_is_not_a_match() {
        let ps = "  201 tool --note=XCONSILIUM_SESSION_TAG=7-1-0 PATH=/bin\n";
        assert_eq!(
            pids_carrying(ps, "CONSILIUM_SESSION_TAG=7-1-0"),
            Vec::<i32>::new()
        );
    }

    #[test]
    fn unreadable_lines_are_skipped() {
        assert_eq!(pids_carrying("", "X=1"), Vec::<i32>::new());
        assert_eq!(pids_carrying("garbage X=1\n", "X=1"), Vec::<i32>::new());
    }

    #[test]
    fn a_finished_run_is_no_longer_tracked() {
        let mut cmd = tokio::process::Command::new("true");
        let reaper = Reaper::fence(&mut cmd);
        let tag = reaper.tag.clone();
        assert!(live_runs().lock().unwrap().contains_key(&tag));
        drop(reaper);
        assert!(!live_runs().lock().unwrap().contains_key(&tag));
    }
}
