use crate::config::VerifyConfig;
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
    let detected = detect_commands(cwd);
    let pick = |label: &str, configured: &Option<String>| -> Option<(String, String)> {
        if let Some(c) = configured {
            return Some((label.to_string(), c.clone()));
        }
        detected
            .iter()
            .find(|(l, _)| l == label)
            .map(|(l, c)| (l.clone(), c.clone()))
    };
    let cfg = cfg.cloned().unwrap_or_default();
    ["build", "test", "lint"]
        .iter()
        .filter_map(|label| {
            let configured = match *label {
                "build" => &cfg.build,
                "test" => &cfg.test,
                _ => &cfg.lint,
            };
            pick(label, configured)
        })
        .collect()
}

/// Runs the resolved commands in `cwd`. Build/test failures set passed=false;
/// lint is advisory (recorded, never blocks). No commands → ran=false.
pub async fn run_verify(cwd: &Path, cfg: Option<&VerifyConfig>) -> VerifyOutcome {
    let cmds = resolve_commands(cwd, cfg);
    if cmds.is_empty() {
        return VerifyOutcome {
            ran: false,
            passed: false,
            summary: "(no build/test/lint command configured or detected)".into(),
        };
    }
    let mut passed = true;
    let mut summary = String::new();
    for (label, cmd) in &cmds {
        let out = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(cwd)
            .output()
            .await;
        match out {
            Ok(o) => {
                let ok = o.status.success();
                let blocking = label != "lint";
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
            Err(e) => {
                // Could not even launch the command. Blocking for build/test;
                // lint stays advisory in EVERY failure mode (a missing linter
                // must not trap a run).
                if label != "lint" {
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
}
