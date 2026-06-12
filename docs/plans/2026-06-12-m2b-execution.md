# Consilium M2b: Execution (conduct + supervisor + auto) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The execution layer completing the product's core: `consilium conduct "<task>"` (conductor decomposes → quota-routed workers edit real files → conductor accepts/reworks → reviewer audits → arbiter on disputes; supervisor gates every step) and `consilium auto "<task>"` (triage → council planning → conduct → optional check command).

**Architecture:** Sequential subtask execution in the shared project cwd (worktree parallelism deferred to v1.2). Workers are write-enabled one-shot sessions (`RunRequest.write`); rework is stateless re-prompting (original prompt + previous diff + feedback), max 2 rounds. Supervisor is a between-step gate (post-subtask), not live injection. All deliberation/JSON parsing reuses M2a machinery (runner, json_extract, prompts/parse pattern, transcript).

**Tech Stack:** existing only (tokio, serde, rusqlite, rand, futures). No new dependencies.

**Repo:** `/Users/temur/Desktop/Claude/consilium`, branch `m2b-execution` off `main`. Baseline: 74 tests green.

**Verified against real CLIs (2026-06-12, probes in a temp git repo — each CLI actually created a file):**

| Provider | Write-enable flag (edits auto-approved, scoped — NOT full bypass) |
|---|---|
| claude | `--permission-mode acceptEdits` |
| codex | `--sandbox workspace-write` |
| gemini | `--approval-mode auto_edit` |

**Scope notes (decided during planning):**
- Subtasks run SEQUENTIALLY in the shared cwd — the conductor is told to design non-overlapping subtasks, but ordering removes conflict risk entirely. Worktree parallelism → v1.2.
- Supervisor verdict is honored as-is (ok/concern/halt); `intervention_threshold` config tuning deferred — document on the struct.
- Effort→CLI-flag mapping still deferred (only codex has a plausible knob via `-c model_reasoning_effort`); not part of M2b.
- `auto`'s integration check = optional `--check "<shell command>"` run by consilium itself after all subtasks (e.g. `cargo test`); workers never run arbitrary commands (their write flags are edit-scoped by design).
- Workers run with `advisory: false` — codex's trusted-dir safeguard stays armed (conduct runs in the user's project, which is a git repo).

---

### Task 1: RunRequest.write + per-CLI write flags (TDD)

**Files:**
- Modify: `core/src/adapters/mod.rs` (add field)
- Modify: `core/src/adapters/{claude,codex,gemini}.rs` (flags + tests)
- Modify: all other RunRequest construction sites (add `write: false`)

- [ ] **Step 1: Add the field** to `RunRequest` in `core/src/adapters/mod.rs`, below `advisory`:

```rust
    /// Write-enabled execution run (conduct workers): the adapter passes its
    /// CLI's scoped auto-approve-edits flag (verified 2026-06-12):
    /// claude `--permission-mode acceptEdits`, codex `--sandbox workspace-write`,
    /// gemini `--approval-mode auto_edit`. Deliberation runs keep this false —
    /// council/review must never mutate files.
    pub write: bool,
```

- [ ] **Step 2: Failing tests (RED).** In each adapter's tests, refactor the existing `build_command` test to a `command_args(advisory: bool, write: bool) -> Vec<String>` helper (codex already has `command_args(advisory)` — extend its signature) and add per adapter:

```rust
    #[test]
    fn write_run_enables_scoped_edits() {
        let args = command_args(false, true);
        // claude: ["--permission-mode", "acceptEdits"]; codex: ["--sandbox", "workspace-write"];
        // gemini: ["--approval-mode", "auto_edit"] — assert the windows(2) pair for THIS adapter.
        assert!(args.windows(2).any(|w| w == ["--permission-mode", "acceptEdits"]));
    }

    #[test]
    fn deliberation_run_has_no_write_flags() {
        let args = command_args(false, false);
        assert!(!args.contains(&"--permission-mode".to_string()));
    }
```

(Adjust the asserted pair per adapter; the negative test asserts absence of `--sandbox` for codex and `--approval-mode` for gemini.)

- [ ] **Step 3: Implement.** In each `build_command`, after existing args:

```rust
        if req.write {
            cmd.arg("--permission-mode").arg("acceptEdits"); // claude
            // codex:  cmd.arg("--sandbox").arg("workspace-write");
            // gemini: cmd.arg("--approval-mode").arg("auto_edit");
        }
```

- [ ] **Step 4: Fix all construction sites** (the compiler drives this): council.rs ×3, review.rs ×1 → `write: false`; main.rs Run arm → `write: false`; roles::request_for → `write: false` (conduct builds its own requests); both test `req()` helpers and the three adapter test helpers → `write: false`.

- [ ] **Step 5: GREEN (80 tests = 74 + 6), fmt, clippy. Commit** `feat: write-enabled runs — scoped auto-edit flags per CLI`

### Task 2: `run` exits non-zero on failure

**Files:** Modify `core/src/main.rs` (Run arm) only.

- [ ] **Step 1:** In the Run arm's event loop, track `let mut failed = false;` — set it in the `Failed` arm. After the loop:

```rust
            if failed {
                std::process::exit(1);
            }
```

- [ ] **Step 2:** Manual smoke without quota spend: `cargo run -q -- run --provider codex "x"` from /tmp (non-git dir) → codex refuses via its own safeguard before any model call; expect `[failed] ...` and exit code 1. Then gates, commit `fix: run exits 1 when the session fails`.

### Task 3: conduct/supervisor/triage contracts + prompts (TDD)

**Files:**
- Modify: `core/src/orchestrator/prompts.rs` (5 new templates)
- Create: `core/src/orchestrator/conduct.rs` (structs + parsers + tests ONLY — orchestration lands in Task 6)
- Modify: `core/src/orchestrator/mod.rs` (`pub mod conduct;`)

- [ ] **Step 1: Failing tests** (bottom of conduct.rs): parse tests + template-drift guards (the M2a pattern):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plan() {
        let text = r#"```json
{"subtasks":[{"id":1,"title":"add module","prompt":"Create src/x.rs with ...","depends_note":""}]}
```"#;
        let plan = parse_plan(text).unwrap();
        assert_eq!(plan.subtasks.len(), 1);
        assert_eq!(plan.subtasks[0].id, 1);
    }

    #[test]
    fn parses_evaluation_variants() {
        for (s, expected) in [
            (r#"{"decision":"accept","feedback":""}"#, EvalDecision::Accept),
            (r#"{"decision":"rework","feedback":"missing tests"}"#, EvalDecision::Rework),
            (r#"{"decision":"fail","feedback":"impossible"}"#, EvalDecision::Fail),
        ] {
            assert_eq!(parse_evaluation(s).unwrap().decision, expected);
        }
    }

    #[test]
    fn unknown_decision_maps_to_rework() {
        // Fail-safe: an unrecognized decision must not auto-accept.
        let v = parse_evaluation(r#"{"decision":"lgtm!","feedback":"x"}"#).unwrap();
        assert_eq!(v.decision, EvalDecision::Rework);
    }

    #[test]
    fn parses_supervisor_verdict() {
        let v = parse_supervisor(r#"{"status":"halt","note":"scope creep"}"#).unwrap();
        assert_eq!(v.status, SupervisorStatus::Halt);
    }

    #[test]
    fn unknown_supervisor_status_maps_to_concern() {
        let v = parse_supervisor(r#"{"status":"hmm","note":""}"#).unwrap();
        assert_eq!(v.status, SupervisorStatus::Concern);
    }

    #[test]
    fn parses_triage() {
        assert!(parse_triage(r#"{"complexity":"trivial"}"#).unwrap().is_trivial());
        assert!(!parse_triage(r#"{"complexity":"standard"}"#).unwrap().is_trivial());
        assert!(!parse_triage(r#"{"complexity":"weird"}"#).unwrap().is_trivial()); // fail-safe: unknown → standard
    }

    #[test]
    fn decompose_template_example_parses_as_plan() {
        let p = crate::orchestrator::prompts::conduct_decompose("t", "ctx");
        assert!(parse_plan(&p).is_some());
    }

    #[test]
    fn evaluation_template_example_parses() {
        let p = crate::orchestrator::prompts::conduct_evaluation("t", "diff", "report", None);
        assert!(parse_evaluation(&p).is_some());
    }

    #[test]
    fn supervisor_template_example_parses() {
        let p = crate::orchestrator::prompts::supervisor_gate("task", "progress");
        assert!(parse_supervisor(&p).is_some());
    }

    #[test]
    fn triage_template_example_parses() {
        let p = crate::orchestrator::prompts::auto_triage("task");
        assert!(parse_triage(&p).is_some());
    }
}
```

- [ ] **Step 2: Implement structs/parsers** (top of conduct.rs) — all via `json_extract`:

```rust
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Subtask {
    pub id: u32,
    #[serde(default)]
    pub title: String,
    pub prompt: String,
    #[serde(default)]
    pub depends_note: String,
}

#[derive(Debug, Deserialize)]
pub struct Plan {
    pub subtasks: Vec<Subtask>,
}

pub fn parse_plan(text: &str) -> Option<Plan> {
    super::json_extract::extract_json_object::<Plan>(text)
}

#[derive(Debug, PartialEq)]
pub enum EvalDecision {
    Accept,
    Rework,
    Fail,
}

#[derive(Debug, Deserialize)]
pub struct Evaluation {
    #[serde(deserialize_with = "lenient_decision", default = "default_decision")]
    pub decision: EvalDecision,
    #[serde(default)]
    pub feedback: String,
}

fn default_decision() -> EvalDecision {
    EvalDecision::Rework
}

// Fail-safe: anything unrecognized becomes Rework — never silent acceptance.
fn lenient_decision<'de, D: serde::Deserializer<'de>>(d: D) -> Result<EvalDecision, D::Error> {
    let s = Option::<String>::deserialize(d)?.unwrap_or_default();
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "accept" => EvalDecision::Accept,
        "fail" => EvalDecision::Fail,
        _ => EvalDecision::Rework,
    })
}

pub fn parse_evaluation(text: &str) -> Option<Evaluation> {
    super::json_extract::extract_json_object::<Evaluation>(text)
}

#[derive(Debug, PartialEq)]
pub enum SupervisorStatus {
    Ok,
    Concern,
    Halt,
}

#[derive(Debug, Deserialize)]
pub struct SupervisorVerdict {
    #[serde(deserialize_with = "lenient_status", default = "default_status")]
    pub status: SupervisorStatus,
    #[serde(default)]
    pub note: String,
}

fn default_status() -> SupervisorStatus {
    SupervisorStatus::Concern
}

// Fail-safe: unknown status is a Concern (logged, surfaced), never silent Ok.
fn lenient_status<'de, D: serde::Deserializer<'de>>(d: D) -> Result<SupervisorStatus, D::Error> {
    let s = Option::<String>::deserialize(d)?.unwrap_or_default();
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "ok" => SupervisorStatus::Ok,
        "halt" => SupervisorStatus::Halt,
        _ => SupervisorStatus::Concern,
    })
}

pub fn parse_supervisor(text: &str) -> Option<SupervisorVerdict> {
    super::json_extract::extract_json_object::<SupervisorVerdict>(text)
}

#[derive(Debug, Deserialize)]
pub struct Triage {
    #[serde(default)]
    complexity: String,
}

impl Triage {
    /// Fail-safe: unknown complexity → standard (full pipeline, never skipped).
    pub fn is_trivial(&self) -> bool {
        self.complexity.trim().eq_ignore_ascii_case("trivial")
    }
}

pub fn parse_triage(text: &str) -> Option<Triage> {
    super::json_extract::extract_json_object::<Triage>(text)
}
```

- [ ] **Step 3: Templates** in prompts.rs (exact texts — product copy):

```rust
pub fn conduct_decompose(task: &str, context: &str) -> String {
    format!(
        "You are the conductor of a team of AI coding agents working in this \
         repository. Decompose the task below into the SMALLEST number of \
         self-contained subtasks (1-5). Each subtask prompt must carry ALL \
         context the worker needs (file paths, conventions, acceptance criteria) \
         — workers cannot see this conversation, each other, or earlier subtasks. \
         Design subtasks so they touch DISJOINT files; they run sequentially.\n\n\
         Task:\n{task}\n\nAdditional context:\n{context}\n\n\
         Output EXACTLY one JSON code block:\n```json\n{{\"subtasks\":[{{\"id\":1,\"title\":\"short name\",\"prompt\":\"full self-contained instructions\",\"depends_note\":\"\"}}]}}\n```"
    )
}

pub fn conduct_evaluation(
    subtask_prompt: &str,
    changes: &str,
    worker_report: &str,
    supervisor_note: Option<&str>,
) -> String {
    let supervisor = supervisor_note
        .map(|n| format!("\nSupervisor's note (weigh it seriously):\n{n}\n"))
        .unwrap_or_default();
    format!(
        "You are the conductor reviewing a worker's completed subtask. Judge \
         ONLY whether the changes fulfil the subtask — not style preferences.\n\n\
         Subtask given to the worker:\n{subtask_prompt}\n\n\
         Changes made (diff + new files):\n<changes>\n{changes}\n</changes>\n\n\
         Worker's report:\n{worker_report}\n{supervisor}\n\
         Output EXACTLY one JSON code block — decision is accept | rework | fail \
         (rework requires concrete, actionable feedback):\n```json\n{{\"decision\":\"accept\",\"feedback\":\"\"}}\n```"
    )
}

pub fn conduct_rework(original_prompt: &str, previous_changes: &str, feedback: &str) -> String {
    format!(
        "A previous attempt at this subtask was rejected. Redo it correctly.\n\n\
         Original subtask:\n{original_prompt}\n\n\
         Previous attempt's changes:\n<changes>\n{previous_changes}\n</changes>\n\n\
         Reviewer feedback to address:\n{feedback}\n\n\
         Apply the fixes on top of the current state of the repository."
    )
}

pub fn supervisor_gate(task: &str, progress: &str) -> String {
    format!(
        "You are the supervisor of a multi-agent coding run. You read a lot and \
         intervene rarely — flag only real problems: scope drift, repeated \
         failures, destructive changes, work that contradicts the task.\n\n\
         Overall task:\n{task}\n\nProgress so far:\n{progress}\n\n\
         Output EXACTLY one JSON code block — status is ok | concern | halt:\n```json\n{{\"status\":\"ok\",\"note\":\"\"}}\n```"
    )
}

pub fn auto_triage(task: &str) -> String {
    format!(
        "Classify this coding task. trivial = single focused change, one file or \
         a couple of lines, no design decisions. standard = everything else.\n\n\
         Task:\n{task}\n\n\
         Output EXACTLY one JSON code block:\n```json\n{{\"complexity\":\"trivial\"}}\n```"
    )
}
```

- [ ] **Step 4: GREEN (80 + 11 = 91), fmt, clippy. Commit** `feat: conduct/supervisor/triage contracts and prompts`

### Task 4: quota-aware worker routing (TDD)

**Files:** Create `core/src/orchestrator/routing.rs` (+ mod.rs, alphabetical)

- [ ] **Step 1: Failing tests:**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RoleConfig;
    use crate::event::Provider;
    use crate::quota::{unix_now, QuotaStore};

    fn role(p: Provider) -> RoleConfig {
        RoleConfig::new(p, "m")
    }

    #[test]
    fn picks_least_loaded_worker() {
        let store = QuotaStore::open_in_memory().unwrap();
        store.record(Provider::Codex, 10_000, 100).unwrap();
        store.record(Provider::Gemini, 50, 5).unwrap();
        let workers = vec![role(Provider::Codex), role(Provider::Gemini)];
        assert_eq!(pick_worker(&workers, &store).unwrap(), 1);
    }

    #[test]
    fn ties_break_by_config_order() {
        let store = QuotaStore::open_in_memory().unwrap();
        let workers = vec![role(Provider::Codex), role(Provider::Gemini)];
        assert_eq!(pick_worker(&workers, &store).unwrap(), 0);
    }

    #[test]
    fn old_usage_outside_window_ignored() {
        let store = QuotaStore::open_in_memory().unwrap();
        store
            .record_at(Provider::Codex, 999_999, 0, unix_now() - 10 * 3600)
            .unwrap();
        store.record(Provider::Gemini, 100, 10).unwrap();
        let workers = vec![role(Provider::Codex), role(Provider::Gemini)];
        assert_eq!(pick_worker(&workers, &store).unwrap(), 0);
    }

    #[test]
    fn empty_workers_is_error() {
        let store = QuotaStore::open_in_memory().unwrap();
        assert!(pick_worker(&[], &store).is_err());
    }
}
```

Note: `RoleConfig::new` is `pub(crate)` — usable here.

- [ ] **Step 2: Implement:**

```rust
use crate::config::RoleConfig;
use crate::quota::{unix_now, QuotaStore};

const ROUTING_WINDOW_SECS: i64 = 5 * 3600;

/// Picks the worker (index into `workers`) whose provider consumed the fewest
/// input tokens in the routing window. Ties break by config order. M2-simple:
/// token volume is a proxy for remaining quota headroom; per-pool $-budgets
/// arrive with M3 dashboards.
pub fn pick_worker(workers: &[RoleConfig], store: &QuotaStore) -> anyhow::Result<usize> {
    if workers.is_empty() {
        anyhow::bail!("no workers configured");
    }
    let since = unix_now() - ROUTING_WINDOW_SECS;
    let mut best = 0usize;
    let mut best_load = u64::MAX;
    for (i, w) in workers.iter().enumerate() {
        let (input, _) = store.totals_since(w.provider, since)?;
        if input < best_load {
            best_load = input;
            best = i;
        }
    }
    Ok(best)
}
```

- [ ] **Step 3: GREEN (95), fmt, clippy. Commit** `feat: quota-aware worker routing`

### Task 5: mutating ScriptedAdapter + git change capture (TDD)

**Files:**
- Modify: `core/tests/common/mod.rs` (add `pre_script` field; add `SequencedAdapter`)
- Create: `core/src/orchestrator/changes.rs` (+ mod.rs)

- [ ] **Step 1: Extend test infra** in common/mod.rs:
  1. Add `pub pre_script: String` to ScriptedAdapter (empty in both constructors; struct-literal users add the field). In `build_command`, prepend it to the sh script: `format!("{}\nsleep {}; cat <<'CONSILIUM_EOF'\n{}\nCONSILIUM_EOF", self.pre_script, self.delay_secs, self.script)`. A fake worker can now REALLY mutate a temp git repo (e.g. `pre_script: "echo hi > out.txt".into()`) before reporting success — conduct tests exercise real change capture with zero quota. IMPORTANT: the fake CLI must run in the request cwd — change `build_command` to also do `cmd.current_dir(&_req.cwd);` (rename `_req` to `req`).
  2. Add `SequencedAdapter`: wraps `Vec<ScriptedAdapter>` + `AtomicUsize` cursor; each `build_command` call uses the NEXT inner adapter (parse_line delegates to ClaudeAdapter as usual). Lets one logical role (conductor) return different scripted responses across sequential calls (plan → verdict → verdict...):

```rust
pub struct SequencedAdapter {
    pub provider: Provider,
    pub steps: Vec<ScriptedAdapter>,
    cursor: std::sync::atomic::AtomicUsize,
}

impl SequencedAdapter {
    pub fn new(provider: Provider, steps: Vec<ScriptedAdapter>) -> Self {
        Self { provider, steps, cursor: std::sync::atomic::AtomicUsize::new(0) }
    }
}

impl Adapter for SequencedAdapter {
    fn provider(&self) -> Provider {
        self.provider
    }
    fn cli_binary(&self) -> &'static str {
        "sh"
    }
    fn build_command(&self, req: &RunRequest) -> tokio::process::Command {
        let i = self
            .cursor
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .min(self.steps.len() - 1); // clamp: repeat last step if over-called
        self.steps[i].build_command(req)
    }
    fn parse_line(&self, line: &str) -> Vec<AgentEvent> {
        ClaudeAdapter.parse_line(line)
    }
}
```

- [ ] **Step 2: Failing tests for change capture** (unit tests in changes.rs, tempfile + std::process git):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &std::path::Path, args: &[&str]) {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success());
    }

    fn temp_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(dir.path(), &["commit", "--allow-empty", "-m", "init", "-q"]);
        dir
    }

    #[test]
    fn captures_tracked_modifications() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("a.txt"), "v1\n").unwrap();
        git(repo.path(), &["add", "."]);
        git(repo.path(), &["commit", "-m", "add a", "-q"]);
        std::fs::write(repo.path().join("a.txt"), "v2\n").unwrap();
        let c = capture_changes(repo.path()).unwrap();
        assert!(c.contains("-v1"));
        assert!(c.contains("+v2"));
    }

    #[test]
    fn captures_untracked_files_with_content() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("new.rs"), "fn x() {}\n").unwrap();
        let c = capture_changes(repo.path()).unwrap();
        assert!(c.contains("new.rs"));
        assert!(c.contains("fn x() {}"));
    }

    #[test]
    fn clean_tree_reports_no_changes() {
        let repo = temp_repo();
        let c = capture_changes(repo.path()).unwrap();
        assert!(c.contains("(no changes)"));
    }

    #[test]
    fn huge_untracked_file_is_capped() {
        let repo = temp_repo();
        std::fs::write(repo.path().join("big.txt"), "x".repeat(100_000)).unwrap();
        let c = capture_changes(repo.path()).unwrap();
        assert!(c.len() < 50_000);
        assert!(c.contains("truncated"));
    }
}
```

- [ ] **Step 3: Implement** `capture_changes(cwd: &Path) -> anyhow::Result<String>`: run `git diff HEAD` (tracked changes), then `git ls-files --others --exclude-standard` and append each untracked file as `--- new file: <path> ---\n<content>` with a per-file cap of 8 KiB chars (append `\n[truncated]` when capped) and a total budget of ~40 KiB chars. Everything empty → `"(no changes)"`. Never touches the git index. Pure std::process, no new deps.

- [ ] **Step 4: GREEN (99 = 95 + 4), fmt, clippy. Commit** `feat: git change capture + mutating scripted adapter`

### Task 6: conduct core — the conductor/worker loop (TDD)

**Files:**
- Modify: `core/src/orchestrator/conduct.rs`
- Create: `core/tests/conduct_test.rs`

Contract (implement exactly):

```rust
pub struct RoleHandle {
    pub adapter: Arc<dyn Adapter>,
    pub model: Option<String>,
}

pub struct ConductDeps {
    pub conductor: RoleHandle,
    pub workers: Vec<CouncilMember>, // reuse label/adapter/model triple
    pub supervisor: Option<RoleHandle>,
}

pub struct ConductOutcome {
    pub completed: Vec<u32>,    // accepted subtask ids, in order
    pub halted: Option<String>, // supervisor halt reason (run aborted)
    pub failed: Option<String>, // conductor fail / rework exhaustion reason
    pub transcript: serde_json::Value,
}

pub const MAX_REWORKS: u32 = 2;

pub async fn run_conduct(
    task: &str,
    context: &str,
    deps: ConductDeps,
    quota: &QuotaStore,
    cwd: PathBuf,
    timeout: Duration,
) -> anyhow::Result<ConductOutcome>
```

Flow:
1. Decompose: conductor session (`advisory: true, write: false` — planning only) with `conduct_decompose`; `parse_plan` None or empty subtasks → bail `"conductor produced no plan"`.
2. For each subtask in order:
   a. `routing::pick_worker(worker role configs...)` — note: workers here are CouncilMember (adapter+model), routing needs providers; map via `member.adapter.provider()`. Adjust: add `pub fn pick_worker_by_provider(providers: &[Provider], store) -> Result<usize>` overload in routing.rs (one-line refactor sharing the core loop) so conduct can route over members directly.
   b. Worker session: `write: true, advisory: false`, prompt = subtask.prompt, cwd.
   c. `changes::capture_changes(&cwd)`.
   d. Supervisor gate (if configured): `supervisor_gate(task, progress)` (advisory) where progress = completed ids + current subtask title + capped changes. Halt → return `halted` + transcript. Concern → note threaded into evaluation.
   e. Conductor evaluation session (advisory) with `conduct_evaluation(subtask.prompt, changes, worker_final_text, supervisor_note)`:
      - Accept → record, next subtask.
      - Rework (or unparseable evaluation — the parse fail-safe) → `conduct_rework` prompt, new worker session, re-capture, re-gate, re-evaluate; after `MAX_REWORKS` → `failed = Some(...)`, stop.
      - Fail → `failed`, stop.
   f. Worker session Failed/TimedOut counts as a rework attempt (feedback = the error text).
3. Transcript: task, plan (titles+ids), per-subtask entries `{id, title, worker, attempts: [{decision, feedback, changes_chars}], supervisor: [{status, note}]}`, completed, halted, failed.

- [ ] **Step 1: Failing integration tests** in `core/tests/conduct_test.rs` — all scripted, zero quota. Conductor = `SequencedAdapter` (different response per call); workers mutate the temp repo via `pre_script`:
  - `happy_path_single_subtask`: conductor = [plan(1 subtask), accept]; worker pre_script creates `out.txt`. Assert completed == [1], `out.txt` exists, transcript has 1 subtask entry with 1 attempt.
  - `rework_then_accept`: conductor = [plan, rework("add more"), accept]; worker steps = [writes v1, appends v2]. Assert completed == [1], 2 attempts with decisions rework→accept in transcript.
  - `rework_exhaustion_fails`: conductor = [plan, rework, rework, rework] → failed.is_some(), completed empty.
  - `supervisor_halt_aborts`: supervisor scripts halt → halted.is_some(), transcript records the halt, no evaluation entry for that subtask.
  - `worker_failure_counts_as_attempt`: worker steps = [failing, ok+creates file]; conductor = [plan, accept] → completed == [1], attempt 1 feedback contains the worker error.

- [ ] **Step 2: Implement** run_conduct. Borrow notes: clone Arcs out of deps before loops; build progress summary incrementally; workers as SequencedAdapter work because conduct re-calls `sessions::spawn` per attempt (each spawn = next scripted step).

- [ ] **Step 3: GREEN (104 = 99 + 5), fmt, clippy. Commit** `feat: conduct — conductor decomposes, workers execute, accept/rework loop`

### Task 7: review-per-subtask + arbiter (TDD)

**Files:** Modify `core/src/orchestrator/conduct.rs`, `core/src/orchestrator/prompts.rs`, `core/tests/conduct_test.rs`

- [ ] **Step 1:** Extend ConductDeps: `pub reviewer: Option<RoleHandle>`, `pub arbiter: Option<RoleHandle>`. After conductor Accept: if reviewer configured → `review::run_review(&changes, ...)` (advisory). `verdict.has_critical()` (or verdict None — fail-closed, consistent with the CLI gate) → forced rework with the findings (or "reviewer output unparseable") as feedback, counting toward MAX_REWORKS. On rework exhaustion with the review gate still failing AND arbiter configured → arbiter session with new template (+ guard test):

```rust
pub fn arbiter_decide(subtask: &str, changes: &str, findings: &str) -> String {
    format!(
        "You are the arbiter. A worker's subtask passed the conductor but the \
         reviewer keeps flagging critical findings after the rework limit. \
         Decide: ship (findings are tolerable or wrong) or fail (findings are \
         real blockers).\n\nSubtask:\n{subtask}\n\nFinal changes:\n<changes>\n{changes}\n</changes>\n\n\
         Reviewer findings:\n{findings}\n\n\
         Output EXACTLY one JSON code block — decision is ship | fail:\n```json\n{{\"decision\":\"ship\",\"reason\":\"\"}}\n```"
    )
}
```

Parse in conduct.rs: `ArbiterVerdict { decision, reason }` with lenient mapping "ship" → Ship, EVERYTHING else → Fail (fail-safe) + unit tests (ship / fail / unknown→fail) + template guard test. No arbiter configured → exhaustion = failed (as in Task 6).

- [ ] **Step 2: Integration tests:** `critical_review_forces_rework` (reviewer = SequencedAdapter [critical findings, clean] → completed after 2 attempts); `arbiter_ships_on_exhaustion` (reviewer always critical, arbiter ships → completed, transcript records arbiter decision+reason); `arbiter_fails_on_exhaustion` (arbiter fails → failed).

- [ ] **Step 3: GREEN (111 = 104 + 4 unit + 3 integration), fmt, clippy. Commit** `feat: per-subtask review gate with arbiter on rework exhaustion`

### Task 8: auto pipeline + CLI arms (TDD)

**Files:**
- Create: `core/src/orchestrator/auto.rs` (+ mod.rs)
- Modify: `core/src/main.rs` (Conduct + Auto subcommands)
- Create: `core/tests/auto_test.rs`

- [ ] **Step 1: auto.rs contract:**

```rust
pub struct AutoDeps {
    pub conduct: ConductDeps,
    pub council_members: Vec<CouncilMember>,
    pub chairman: RoleHandle,
}

pub struct AutoOutcome {
    pub triage_trivial: bool,
    pub council_synthesis: Option<String>,
    pub conduct: ConductOutcome,
    pub check: Option<(bool, String)>, // (passed, output tail)
    pub transcript: serde_json::Value,
}

pub async fn run_auto(
    task: &str,
    deps: AutoDeps,
    quota: &QuotaStore,
    cwd: PathBuf,
    timeout: Duration,
    check_command: Option<&str>,
) -> anyhow::Result<AutoOutcome>
```

Flow: triage via conductor adapter (`auto_triage`, advisory; parse fail-safe → standard) → trivial: `run_conduct(task, "", ...)` — standard: `council::run_council` on `format!("How should we approach this coding task? Outline the plan, key files, and risks.\n\nTask: {task}")` → synthesis becomes conduct's `context` → `run_conduct` → if fully completed (no halted/failed) and `check_command` given: run via `sh -c` in cwd (std::process), capture success + last 2 KiB of combined output. Transcript composes child transcripts.

- [ ] **Step 2: Failing integration tests** (`core/tests/auto_test.rs`, scripted):
  - `trivial_skips_council`: conductor = [triage trivial, plan, accept]; council members = ScriptedAdapter::failing (if council ran, run_council would bail and run_auto would error — passing test proves the skip). Assert triage_trivial, council_synthesis.is_none(), conduct completed.
  - `standard_runs_council_then_conduct`: conductor = [triage standard, plan, accept]; council members/chairman scripted ok. Assert council_synthesis.is_some(), completed.
  - `check_command_failure_reported`: trivial flow + check_command = Some("false") → check == Some((false, _)).

- [ ] **Step 3: CLI arms.** `Conduct { task, #[arg(long)] context: Option<String>, #[arg(long, default_value_t = 900)] timeout: u64 }` and `Auto { task, #[arg(long)] check: Option<String>, #[arg(long, default_value_t = 900)] timeout: u64 }`. Deps from config: conductor=roles.conductor, workers=roles.workers (as CouncilMembers, label "{provider}-{model}"), supervisor=Some(roles.supervisor), reviewer=Some(roles.reviewer), arbiter=Some(roles.chairman); council members/chairman as in the M2a council arm. Output: print completed ids / halted / failed / check result + transcript path (kinds "conduct"/"auto"). Exit codes: 0 full success (and check passed if given), 1 otherwise.

- [ ] **Step 4: GREEN (114 = 111 + 3), fmt, clippy, `--help` smokes for both. Commit** `feat: auto pipeline and conduct/auto CLI commands`

### Task 9: real E2E smoke + README (sanctioned quota spend)

- [ ] **Step 1:** Scratch repo: `/tmp/consilium-conduct-smoke` (git init, README.md with a deliberate typo, commit). Run `consilium conduct "Add a CHANGELOG.md file with a single entry: 0.1.0 — initial release" --timeout 600`. Expect: 1-subtask plan, worker really creates CHANGELOG.md, conductor accepts, reviewer audits, exit 0. Inspect transcript + `git status`.
- [ ] **Step 2:** `consilium auto "Fix the typo in README.md" --check "test -f README.md" --timeout 600`. Expect triage (likely trivial → council skipped), fix applied, check passes, exit 0.
- [ ] **Step 3:** Reality bites (flag drift, prompt non-compliance) → fix adapters/prompts, keep suite green, re-smoke. Report findings explicitly.
- [ ] **Step 4:** README: status table M2b ✅; conduct/auto in Quick start. Final gates. Commit `feat: M2b execution complete — conduct and auto verified on real providers`

---

## M2b Exit Criteria

- `cargo test` green (≥110 tests), clippy `-D warnings`, fmt clean; the test suite spends zero quota.
- `consilium conduct` completes a real task end-to-end: real file changes by a worker CLI, conductor acceptance, review gate, transcript with full attempt history.
- `consilium auto` triages, (optionally) councils, conducts, runs the check command; exit codes 0/1 correct.
- `consilium run` exits 1 on session failure.
- Codex's trusted-dir safeguard stays armed for all write runs (`advisory: false`); write access is the scoped `--sandbox workspace-write` only.

## After M2b

**M3:** axum server (REST+WS), MCP attached mode (rmcp) — conductor inside the user's interactive Claude Code session, React web UI, quota-$ dashboards. v1.1+: Warp adapter (OSC 777), Tauri, Claude Code skill.
