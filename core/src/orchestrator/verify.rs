use crate::config::VerifyConfig;
use crate::safety::{resolve_commands_with_provenance, VerificationCommand};
use std::path::Path;

/// Structured result of running the worktree's build/test/lint commands.
#[derive(Debug, Clone)]
pub struct VerifyOutcome {
    /// At least one command was resolved and executed.
    pub ran: bool,
    /// True iff every BLOCKING command (build, test) succeeded. Lint is advisory.
    pub passed: bool,
    /// Per-command outcomes for the conductor + transcript (capped).
    pub summary: String,
}

const TAIL_CAP: usize = 3000;

/// Per-command wall-clock cap (seconds) when `verify.timeoutSecs` is unset.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Resolve the per-command timeout: `verify.timeoutSecs` when set, else the
/// 600s default. Shared by `run_verify` and auto's `--check` command.
pub fn command_timeout(cfg: Option<&VerifyConfig>) -> std::time::Duration {
    std::time::Duration::from_secs(
        cfg.and_then(|c| c.timeout_secs)
            .unwrap_or(DEFAULT_TIMEOUT_SECS),
    )
}

fn truncate_tail(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let start = s.len() - max;
    let mut i = start;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

/// Ecosystem auto-detection by repo marker files. Empty = nothing recognized.
pub fn detect_commands(cwd: &Path) -> Vec<(String, String)> {
    let mut cmds = Vec::new();
    if cwd.join("Cargo.toml").exists() {
        cmds.push(("build".into(), "cargo build".into()));
        cmds.push(("test".into(), "cargo test".into()));
        cmds.push((
            "lint".into(),
            "cargo clippy --all-targets -- -D warnings".into(),
        ));
    } else if cwd.join("package.json").exists() {
        cmds.push(("test".into(), "npm test --silent".into()));
    } else if cwd.join("pyproject.toml").exists() || cwd.join("pytest.ini").exists() {
        cmds.push(("test".into(), "pytest -q".into()));
    } else if cwd.join("Makefile").exists() {
        cmds.push(("test".into(), "make test".into()));
    }
    cmds
}

/// Config commands win per-field; unspecified fields fall back to detection.
pub fn resolve_commands(cwd: &Path, cfg: Option<&VerifyConfig>) -> Vec<(String, String)> {
    resolve_commands_with_provenance(cwd, cfg)
        .into_iter()
        .map(|item| (item.label, item.command))
        .collect()
}

/// Runs the resolved commands in `cwd`. Build/test failures set passed=false;
/// lint is advisory (recorded, never blocks). No commands → ran=false.
/// Each command is capped at `verify.timeoutSecs` (default 600s): on expiry
/// the child is SIGKILLed and the command is recorded as TIMEOUT, so a
/// worker-introduced hanging test/build cannot stall the whole run.
pub async fn run_verify(cwd: &Path, cfg: Option<&VerifyConfig>) -> VerifyOutcome {
    let commands = resolve_commands_with_provenance(cwd, cfg);
    run_resolved_verify(cwd, &commands).await
}

/// Runs commands that were already resolved and disclosed by the safety
/// preflight, preserving their provenance and per-command timeout.
pub async fn run_resolved_verify(cwd: &Path, commands: &[VerificationCommand]) -> VerifyOutcome {
    if commands.is_empty() {
        return VerifyOutcome {
            ran: false,
            passed: false,
            summary: "(no build/test/lint command configured or detected)".into(),
        };
    }
    let mut passed = true;
    let mut summary = String::new();
    for item in commands {
        let label = &item.label;
        let cmd = &item.command;
        let timeout = std::time::Duration::from_secs(item.timeout_secs);
        let blocking = label != "lint";
        let out = tokio::time::timeout(
            timeout,
            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(cwd)
                .kill_on_drop(true)
                .output(),
        )
        .await;
        // On Err(Elapsed) the output() future — and with it the kill_on_drop
        // child — is dropped at the end of the statement above, so SIGKILL is
        // issued BEFORE the next command runs (or the caller reuses the cwd).
        match out {
            Err(_elapsed) => {
                // Timed out. Blocking for build/test; lint stays advisory in
                // EVERY failure mode (a hanging linter must not trap a run).
                if blocking {
                    passed = false;
                }
                let marker = if blocking {
                    "TIMEOUT"
                } else {
                    "timeout (advisory)"
                };
                summary.push_str(&format!(
                    "[{label}] {marker}: {cmd} (exceeded {}s; process killed)\n",
                    timeout.as_secs()
                ));
            }
            Ok(Ok(o)) => {
                let ok = o.status.success();
                if !ok && blocking {
                    passed = false;
                }
                let marker = if ok {
                    "ok"
                } else if blocking {
                    "FAILED"
                } else {
                    "failed (advisory)"
                };
                summary.push_str(&format!("[{label}] {marker}: {cmd}\n"));
                if !ok {
                    let combined = format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    );
                    summary.push_str(truncate_tail(combined.trim(), TAIL_CAP));
                    summary.push('\n');
                }
            }
            Ok(Err(e)) => {
                // Could not even launch the command. Blocking for build/test;
                // lint stays advisory in EVERY failure mode (a missing linter
                // must not trap a run).
                if blocking {
                    passed = false;
                }
                summary.push_str(&format!("[{label}] LAUNCH-ERROR: {cmd}: {e}\n"));
            }
        }
    }
    VerifyOutcome {
        ran: true,
        passed,
        summary: summary.trim_end().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::VerifyConfig;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn detects_cargo_repo() {
        let d = tmp();
        std::fs::write(d.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let cmds = detect_commands(d.path());
        assert!(cmds
            .iter()
            .any(|(label, cmd)| label == "test" && cmd.contains("cargo test")));
        assert!(cmds
            .iter()
            .any(|(label, cmd)| label == "build" && cmd.contains("cargo build")));
    }

    #[test]
    fn detects_npm_repo() {
        let d = tmp();
        std::fs::write(d.path().join("package.json"), "{\"name\":\"x\"}").unwrap();
        let cmds = detect_commands(d.path());
        assert!(cmds.iter().any(|(label, _)| label == "test"));
    }

    #[test]
    fn empty_dir_detects_nothing() {
        let d = tmp();
        assert!(detect_commands(d.path()).is_empty());
    }

    #[test]
    fn config_commands_override_detection() {
        let d = tmp();
        std::fs::write(d.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let cfg = VerifyConfig {
            test: Some("echo configured-test".into()),
            build: None,
            lint: None,
            timeout_secs: None,
        };
        let cmds = resolve_commands(d.path(), Some(&cfg));
        // configured test wins; build/lint fall back to cargo detection
        assert!(cmds
            .iter()
            .any(|(l, c)| l == "test" && c == "echo configured-test"));
        assert!(cmds
            .iter()
            .any(|(l, c)| l == "build" && c.contains("cargo build")));
    }

    #[tokio::test]
    async fn run_verify_passes_when_commands_succeed() {
        let d = tmp();
        let cfg = VerifyConfig {
            test: Some("true".into()),
            build: Some("true".into()),
            lint: None,
            timeout_secs: None,
        };
        let out = run_verify(d.path(), Some(&cfg)).await;
        assert!(out.ran);
        assert!(out.passed);
    }

    #[tokio::test]
    async fn run_verify_fails_when_test_fails() {
        let d = tmp();
        let cfg = VerifyConfig {
            test: Some("false".into()),
            build: None,
            lint: None,
            timeout_secs: None,
        };
        let out = run_verify(d.path(), Some(&cfg)).await;
        assert!(out.ran);
        assert!(!out.passed);
        assert!(out.summary.contains("test"));
    }

    #[tokio::test]
    async fn run_verify_lint_failure_does_not_block() {
        let d = tmp();
        // lint fails but test passes → passed (lint is advisory)
        let cfg = VerifyConfig {
            test: Some("true".into()),
            build: None,
            lint: Some("false".into()),
            timeout_secs: None,
        };
        let out = run_verify(d.path(), Some(&cfg)).await;
        assert!(out.ran);
        assert!(out.passed, "lint failure must not block accept");
        assert!(out.summary.contains("lint"));
    }

    #[tokio::test]
    async fn run_verify_lint_launch_error_does_not_block() {
        let d = tmp();
        // lint binary does not exist -> launch error, but lint is advisory
        let cfg = VerifyConfig {
            test: Some("true".into()),
            build: None,
            lint: Some("consilium-no-such-linter-xyz".into()),
            timeout_secs: None,
        };
        let out = run_verify(d.path(), Some(&cfg)).await;
        assert!(out.ran);
        assert!(out.passed, "a missing linter must not block accept");
    }

    #[tokio::test]
    async fn run_verify_reports_not_run_when_no_commands() {
        let d = tmp(); // empty, no config
        let out = run_verify(d.path(), None).await;
        assert!(!out.ran);
        assert!(!out.passed); // not-run is not a pass
    }

    #[test]
    fn command_timeout_resolves_config_or_default() {
        assert_eq!(
            command_timeout(None),
            std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS)
        );
        let cfg = VerifyConfig {
            timeout_secs: Some(5),
            ..Default::default()
        };
        assert_eq!(
            command_timeout(Some(&cfg)),
            std::time::Duration::from_secs(5)
        );
        let unset = VerifyConfig::default();
        assert_eq!(
            command_timeout(Some(&unset)),
            std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS)
        );
    }

    // A hanging verify command must not stall the run forever: it is killed at
    // the cap and reported as a FAILED (timed-out) verify, and the child is
    // SIGKILLed so it cannot keep mutating the cwd afterwards.
    #[tokio::test]
    async fn run_verify_timeout_kills_hanging_command() {
        let d = tmp();
        let marker = d.path().join("late.txt");
        let cfg = VerifyConfig {
            test: Some("sleep 2; echo late > late.txt".into()),
            timeout_secs: Some(1),
            ..Default::default()
        };
        let out = run_verify(d.path(), Some(&cfg)).await;
        assert!(out.ran);
        assert!(!out.passed, "a timed-out test command must fail verify");
        assert!(
            out.summary.contains("TIMEOUT"),
            "summary must say TIMEOUT, got: {}",
            out.summary
        );

        // Well past the child's 2s sleep: an orphaned child would have written
        // the marker by now; a killed one never will.
        tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
        assert!(
            !marker.exists(),
            "timed-out verify child must be SIGKILLed before it can write"
        );

        // Positive control: the SAME script, given time to finish, DOES write
        // the marker — proving the suppression above is the kill.
        let d2 = tmp();
        let cfg2 = VerifyConfig {
            test: Some("sleep 2; echo late > late.txt".into()),
            timeout_secs: Some(30),
            ..Default::default()
        };
        let out2 = run_verify(d2.path(), Some(&cfg2)).await;
        assert!(out2.passed, "un-killed script must pass");
        assert!(
            d2.path().join("late.txt").exists(),
            "an un-killed verify command completes its write (positive control)"
        );
    }

    #[tokio::test]
    async fn run_verify_lint_timeout_is_advisory() {
        let d = tmp();
        // lint hangs past the cap but test passes → passed (lint is advisory
        // in EVERY failure mode, including timeout).
        let cfg = VerifyConfig {
            test: Some("true".into()),
            lint: Some("sleep 2".into()),
            timeout_secs: Some(1),
            ..Default::default()
        };
        let out = run_verify(d.path(), Some(&cfg)).await;
        assert!(out.ran);
        assert!(out.passed, "a lint timeout must not block accept");
        assert!(
            out.summary.contains("timeout (advisory)"),
            "summary must record the advisory lint timeout, got: {}",
            out.summary
        );
    }
}
