# P0: Build/Test Grounding — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Ground conduct's accept/rework in real build/test/lint results instead of the conductor judging text alone. A worker change whose tests fail can no longer be Accepted; "no verifier ran" is recorded as an explicit unverified signal.

**Why (from research, 2026-06-17):** intrinsic self-correction without an external verifier does not reliably improve and often degrades (Kamoi TACL 2024, Huang ICLR 2024). The conductor's opinion becomes a tiebreaker *on top of* the test signal — never a substitute. This is the keystone P0 item; ConductorMemory and the worker blackboard (the other two P0 items) build on a grounded loop. See `docs/research/2026-06-17-harness-leveling-research.md`.

**Architecture:** A new `verify` module runs build/test/lint commands (config-declared, else auto-detected per ecosystem) in the worktree after `capture_changes`, returning a structured `VerifyOutcome`. `run_conduct` feeds that outcome into the evaluation prompt and applies one hard rule: **verify ran and failed → the subtask cannot be Accepted this attempt (forced Rework with the failure as feedback).** Verify passed or did-not-run → the conductor decides as before, with the verify status recorded in the transcript.

**Tech Stack:** existing only (tokio, serde, std::process). No new deps.

**Repo:** `/Users/temur/Desktop/Claude/consilium`, branch `m4-grounding` off `main`. Baseline: 151 tests green.

**Scope notes (decided):**
- Detection is config-first, auto-detect fallback, then unverified. No silent magic.
- Per-subtask verify is the new grounding; `auto`'s existing end-of-run `--check` stays unchanged (a final whole-task gate, complementary).
- ConductorMemory (P0 #2) and worker blackboard (P0 #3) are SEPARATE follow-on plans — not in scope here.
- Verify runs build+test+lint; lint failure is advisory (recorded, not Accept-blocking); only **test or build failure blocks Accept** (lint noise should not trap a run). Documented in code.

---

### Task 1: VerifyConfig + verify module (TDD)

**Files:**
- Modify: `core/src/config.rs` (add `VerifyConfig` + `Config.verify`)
- Create: `core/src/orchestrator/verify.rs`
- Modify: `core/src/orchestrator/mod.rs` (`pub mod verify;`, alphabetical: after transcript? no — after `roles`/`routing`/`runner`/`transcript` — `verify` sorts last)

- [ ] **Step 1: Config — add VerifyConfig.** In `config.rs`, after `QuotaConfig`:

```rust
/// Explicit build/test/lint commands for grounding conduct's accept/rework.
/// Any field None falls back to ecosystem auto-detection; if neither yields a
/// command, that check is skipped (recorded as "did not run").
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VerifyConfig {
    #[serde(default)]
    pub build: Option<String>,
    #[serde(default)]
    pub test: Option<String>,
    #[serde(default)]
    pub lint: Option<String>,
}
```

Add to `Config`: `#[serde(default)] pub verify: Option<VerifyConfig>,` (after `quota`). Update the `Config::default()` literal to add `verify: None,`. Update `default_config_round_trips_through_json` is unaffected (verify defaults None). Run `cargo test -p consilium config` → existing 6 config tests still pass (serde default makes old JSON parse). Add a test `verify_config_parses`:

```rust
#[test]
fn verify_config_parses() {
    let json = r#"{"roles":{"conductor":{"provider":"claude","model":"m"},
        "chairman":{"provider":"claude","model":"m"},"workers":[],
        "reviewer":{"provider":"codex","model":"m"},
        "supervisor":{"provider":"gemini","model":"m"}},
        "verify":{"test":"cargo test","build":"cargo build"}}"#;
    let cfg: Config = serde_json::from_str(json).unwrap();
    let v = cfg.verify.unwrap();
    assert_eq!(v.test.as_deref(), Some("cargo test"));
    assert_eq!(v.build.as_deref(), Some("cargo build"));
    assert!(v.lint.is_none());
}
```

- [ ] **Step 2: Failing tests for verify.rs** (bottom of new `core/src/orchestrator/verify.rs`):

```rust
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
        assert!(cmds.iter().any(|(label, cmd)| label == "test" && cmd.contains("cargo test")));
        assert!(cmds.iter().any(|(label, cmd)| label == "build" && cmd.contains("cargo build")));
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
        let cfg = VerifyConfig { test: Some("echo configured-test".into()), build: None, lint: None };
        let cmds = resolve_commands(d.path(), Some(&cfg));
        // configured test wins; build/lint fall back to cargo detection
        assert!(cmds.iter().any(|(l, c)| l == "test" && c == "echo configured-test"));
        assert!(cmds.iter().any(|(l, c)| l == "build" && c.contains("cargo build")));
    }

    #[tokio::test]
    async fn run_verify_passes_when_commands_succeed() {
        let d = tmp();
        let cfg = VerifyConfig { test: Some("true".into()), build: Some("true".into()), lint: None };
        let out = run_verify(d.path(), Some(&cfg)).await;
        assert!(out.ran);
        assert!(out.passed);
    }

    #[tokio::test]
    async fn run_verify_fails_when_test_fails() {
        let d = tmp();
        let cfg = VerifyConfig { test: Some("false".into()), build: None, lint: None };
        let out = run_verify(d.path(), Some(&cfg)).await;
        assert!(out.ran);
        assert!(!out.passed);
        assert!(out.summary.contains("test"));
    }

    #[tokio::test]
    async fn run_verify_lint_failure_does_not_block() {
        let d = tmp();
        // lint fails but test passes → passed (lint is advisory)
        let cfg = VerifyConfig { test: Some("true".into()), build: None, lint: Some("false".into()) };
        let out = run_verify(d.path(), Some(&cfg)).await;
        assert!(out.ran);
        assert!(out.passed, "lint failure must not block accept");
        assert!(out.summary.contains("lint"));
    }

    #[tokio::test]
    async fn run_verify_reports_not_run_when_no_commands() {
        let d = tmp(); // empty, no config
        let out = run_verify(d.path(), None).await;
        assert!(!out.ran);
        assert!(!out.passed); // not-run is not a pass
    }
}
```

- [ ] **Step 3: Run, confirm COMPILE ERROR (RED).** `cargo test -p consilium verify`.

- [ ] **Step 4: Implement** (top of `verify.rs`):

```rust
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
        cmds.push(("lint".into(), "cargo clippy --all-targets -- -D warnings".into()));
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
                let marker = if ok { "ok" } else if blocking { "FAILED" } else { "failed (advisory)" };
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
                // Could not even launch the command — blocking, treat as failure.
                passed = false;
                summary.push_str(&format!("[{label}] LAUNCH-ERROR: {cmd}: {e}\n"));
            }
        }
    }
    VerifyOutcome { ran: true, passed, summary: summary.trim_end().to_string() }
}
```

Add `pub mod verify;` to `core/src/orchestrator/mod.rs` (alphabetical — last).

- [ ] **Step 5: GREEN.** `cargo test -p consilium` → 158 (151 + 1 config + 6 verify). `cargo fmt --all`; check clippy real exit (`cargo clippy --all-targets -- -D warnings` standalone, NOT piped through tail). Commit `feat: verify module — config/auto-detected build/test/lint runner`.

### Task 2: conduct_evaluation prompt carries the verify result (TDD)

**Files:** Modify `core/src/orchestrator/prompts.rs`, `core/src/orchestrator/conduct.rs` (prompt-drift guard test).

- [ ] **Step 1:** Change `conduct_evaluation` to accept a verify summary and surface it prominently. New signature: add `verify: &str` parameter (caller passes the VerifyOutcome.summary, or a "(not run)" marker). Insert a `<verify>` block AND an explicit instruction:

```rust
pub fn conduct_evaluation(
    subtask_prompt: &str,
    changes: &str,
    worker_report: &str,
    verify: &str,
    supervisor_note: Option<&str>,
) -> String {
    let supervisor = supervisor_note
        .map(|n| format!("\nSupervisor's note (weigh it seriously):\n{n}\n"))
        .unwrap_or_default();
    format!(
        "You are the conductor reviewing a worker's completed subtask. Judge \
         whether the changes fulfil the subtask. Build/test results are AUTHORITATIVE: \
         if tests or build failed, you must NOT accept — request rework citing the \
         failure. If no verifier ran, treat your judgment as unverified and be \
         conservative.\n\n\
         Subtask given to the worker:\n{subtask_prompt}\n\n\
         Changes made (diff + new files):\n<changes>\n{changes}\n</changes>\n\n\
         Build/test/lint result:\n<verify>\n{verify}\n</verify>\n\n\
         Worker's report:\n<worker_report>\n{worker_report}\n</worker_report>\n{supervisor}\n\
         Output EXACTLY one JSON code block — decision is accept | rework | fail \
         (rework requires concrete, actionable feedback):\n```json\n{{\"decision\":\"accept\",\"feedback\":\"\"}}\n```"
    )
}
```

- [ ] **Step 2:** Update the existing prompt-drift guard test `evaluation_template_example_parses` in conduct.rs to pass the new `verify` arg (e.g. `conduct_evaluation("t", "diff", "report", "(not run)", None)`). Run `cargo test -p consilium` → compile error at the conduct.rs call site of `conduct_evaluation` (real wiring) — that's expected; Task 3 fixes the call site. For THIS task, only the prompt fn + its guard test change; if conduct.rs won't compile until Task 3, combine: do the conduct.rs call-site update minimally here so the tree compiles (pass the current `(not run)` placeholder), and Task 3 replaces the placeholder with the real VerifyOutcome. Simplest: make this task ALSO thread a `"(not run)"` placeholder at the single call site so everything compiles and all 158 tests stay green.

- [ ] **Step 3:** `cargo fmt`; clippy (real exit); `cargo test` 158 green. Commit `feat: conduct evaluation prompt treats build/test as authoritative`.

### Task 3: wire verify into run_conduct — grounding rule + transcript (TDD)

**Files:** Modify `core/src/orchestrator/conduct.rs`; `core/tests/conduct_test.rs`.

- [ ] **Step 1: Failing integration tests** in conduct_test.rs (scripted, zero quota; workers really mutate a temp git repo via pre_script; verify uses cheap shell commands via a config passed through ConductDeps — see Step 2 for how verify config reaches run_conduct):

```rust
#[tokio::test]
async fn failing_tests_force_rework_even_if_conductor_would_accept() {
    let repo = temp_repo();
    let quota = store();
    // conductor: plan, then accept twice (it WOULD accept both attempts)
    let conductor = Arc::new(SequencedAdapter::new(
        Provider::Claude,
        vec![
            ScriptedAdapter::ok_with_text(Provider::Claude, &plan_json(&[(1, "x", "write out.txt")])),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
            ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
        ],
    ));
    // worker attempt 1: writes a marker file but "tests" fail; attempt 2: writes the pass marker
    let worker = Arc::new(SequencedAdapter::new(
        Provider::Codex,
        vec![
            ScriptedAdapter { pre_script: "echo bad > out.txt".into(), ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it") },
            ScriptedAdapter { pre_script: "echo good > out.txt".into(), ..ScriptedAdapter::ok_with_text(Provider::Codex, "fixed it") },
        ],
    ));
    // verify: a test command that passes only when out.txt contains "good"
    let verify = VerifyConfig { test: Some("grep -q good out.txt".into()), build: None, lint: None };
    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![CouncilMember { label: "codex".into(), ladder: vec![Rung { candidate: ModelCandidate { provider: Provider::Codex, model: "m".into() }, adapter: worker }] }],
        supervisor: None, reviewer: None, arbiter: None,
        verify: Some(verify),
    };
    let outcome = run_conduct("t", "", deps, &quota, repo.path().to_path_buf(), TIMEOUT, &health()).await.unwrap();
    assert_eq!(outcome.completed, vec![1]);
    // attempt 1 must be a rework caused by verify, not an accept
    let attempts = outcome.transcript["subtasks"][0]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["decision"], "rework");
    assert_eq!(attempts[0]["verify"], "failed");
    assert_eq!(attempts[1]["decision"], "accept");
    assert_eq!(attempts[1]["verify"], "passed");
}

#[tokio::test]
async fn no_verifier_is_recorded_as_unverified() {
    let repo = temp_repo();
    let quota = store();
    let conductor = Arc::new(SequencedAdapter::new(Provider::Claude, vec![
        ScriptedAdapter::ok_with_text(Provider::Claude, &plan_json(&[(1, "x", "write out.txt")])),
        ScriptedAdapter::ok_with_text(Provider::Claude, &accept_json()),
    ]));
    let worker = Arc::new(ScriptedAdapter { pre_script: "echo hi > out.txt".into(), ..ScriptedAdapter::ok_with_text(Provider::Codex, "did it") });
    let deps = ConductDeps {
        conductor: solo_role_handle(Provider::Claude, "m", conductor),
        workers: vec![solo_worker("codex", Provider::Codex, "m", worker)],
        supervisor: None, reviewer: None, arbiter: None,
        verify: None, // no config; temp repo has no Cargo.toml etc.
    };
    let outcome = run_conduct("t", "", deps, &quota, repo.path().to_path_buf(), TIMEOUT, &health()).await.unwrap();
    assert_eq!(outcome.completed, vec![1]);
    let attempts = outcome.transcript["subtasks"][0]["attempts"].as_array().unwrap();
    assert_eq!(attempts[0]["verify"], "not_run");
}
```

ALSO: every existing ConductDeps construction in conduct_test.rs + auto.rs call sites gains `verify: None` (compiler will list them).

- [ ] **Step 2: Implement.** Add `pub verify: Option<VerifyConfig>` to `ConductDeps`. In `run_conduct`, after `capture_changes(&cwd)`:
  - `let verify_outcome = verify::run_verify(&cwd, deps.verify.as_ref()).await;` (clone `deps.verify` out before the loop like other deps).
  - Pass `&verify_outcome.summary` (or `"(not run)"` if `!ran`) into `conduct_evaluation`.
  - After parsing the evaluation, apply the grounding rule: `if verify_outcome.ran && !verify_outcome.passed && evaluation.decision == EvalDecision::Accept { override to Rework, feedback = format!("Build/test failed; fix before acceptance:\n{}", verify_outcome.summary) }`.
  - Record per attempt: `"verify": match (ran, passed) { (false,_) => "not_run", (true,true) => "passed", (true,false) => "failed" }`.
  - The forced-rework still counts toward MAX_REWORKS (it's a real attempt).
- [ ] **Step 3:** Move the `verify_outcome` computation so it happens for EACH attempt (re-run after each worker attempt's capture_changes), not once.
- [ ] **Step 4: GREEN.** `cargo test` (158 + 2 = 160). fmt; clippy (real exit). Commit `feat: conduct grounds accept/rework in build/test results`.

### Task 4: CLI wiring + init + real dogfood smoke

**Files:** Modify `core/src/main.rs` (Conduct/Auto arms load `config.verify` into deps; `init` default config includes a commented verify hint), real smoke.

- [ ] **Step 1:** In main.rs Conduct + Auto arms, set `verify: config.verify.clone()` on the built ConductDeps. (Auto builds ConductDeps inside AutoDeps — thread it there too.)
- [ ] **Step 2:** Build. `cargo run -q -- conduct --help` still works. Full gates: fmt, clippy (real exit), `cargo test` (160).
- [ ] **Step 3: Real dogfood smoke (sanctioned quota).** In a scratch git repo with a trivial Rust crate (Cargo.toml + a lib.rs with one passing test), write `consilium.config.json` with `"verify":{"test":"cargo test"}` and a conductor ladder of `claude-opus-4-8`. Run `consilium conduct "add a function add(a,b)->i64 with a passing unit test to src/lib.rs"`. Expect: worker writes code, verify runs `cargo test`, conductor accepts only when tests pass; if the worker's first cut fails to compile, observe a verify-forced rework in the transcript. Capture the transcript showing `"verify":"passed"`/`"failed"`.
- [ ] **Step 4:** README: add a short "Grounded execution" note (conduct runs your build/test and won't accept a subtask whose tests fail; configure via `verify` in consilium.config.json or rely on auto-detection). Commit `feat: P0 grounding wired end-to-end — verified on a real cargo repo`.

---

## P0-Grounding Exit Criteria

- `cargo test` green (≥160), clippy `-D warnings` clean (verified by real exit, not piped), fmt clean; zero quota in the suite.
- A conduct subtask whose tests fail is forced to Rework even when the conductor's text says "accept"; the override and the verify status are in the transcript.
- "No verifier ran" is recorded as `not_run` and the conductor is told its judgment is unverified.
- Real dogfood: a `cargo test`-verified conduct run on a scratch crate, transcript shows the verify gating.

## Next P0 slices (separate plans, after this lands)

- **ConductorMemory** (P0 #2): live plan ledger + folded worker summaries replacing the dead transcript, hydrated per stage.
- **Worker blackboard** (P0 #3): append-only structured artifacts so worker N inherits workers 1..N-1's learnings; worktree isolation.
