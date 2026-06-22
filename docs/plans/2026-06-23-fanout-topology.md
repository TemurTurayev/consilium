# Fan-out / Parallel-Worker Topology Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `conduct` a real subtask **DAG** — explicit `depends_on` edges, dependency-ordered execution, and failure isolation (a failed subtask skips only its dependents, not the whole run) — as the safe, sequential foundation for later true parallelism.

**Architecture:** Add a structured `depends_on: Vec<u32>` edge to `Subtask`; a pure `topology::plan_waves` groups subtasks into dependency layers (Kahn's algorithm); `run_conduct` iterates waves instead of raw plan order; a subtask whose prerequisites did not complete is recorded `skipped` without running a worker; and a subtask failure no longer aborts the whole plan — it isolates to its (transitive) dependents while independent work proceeds. Execution stays **fully sequential** in this plan — one worker at a time, shared `cwd` — so there is zero concurrency risk. True per-wave parallelism (worktree isolation, concurrent workers, streaming attribution) is **Phase B**, deferred to its own brainstorm + plan because its design is not settled (see the Phase B section).

**Tech Stack:** Rust (edition 2021), crate `consilium` (in `core/`), `serde`/`serde_json`, `anyhow`, `tokio`. Tests are `#[test]`/`#[tokio::test]` co-located in each module. No new dependencies.

---

## Status & scope

*Status: DRAFT for review (2026-06-23). Foundation phase of the fan-out topology (memory `project_consilium.md`, task #72). Sequential-only — no worktrees, no concurrency.*

This plan delivers **slices 1 + 2** of the five sliced in memory:

1. ✅ (this plan) `depends_on` + topological layering — *the "safe" slice.*
2. ✅ (this plan) skip-failed-dependency — *which requires the failure-model change below.*
3. ⏸ (Phase B) worktree isolation per worker.
4. ⏸ (Phase B) parallel execution per wave.
5. ⏸ (Phase B) streaming attribution (member_id + sink inheritance).

## Problem (confirmed against the code, not assumed)

`run_conduct` ([conduct.rs:312-758](../../core/src/orchestrator/conduct.rs)) executes subtasks in **raw plan order**, fully sequentially: `'subtask: for subtask in &plan.subtasks`. Two gaps follow:

- **No structured dependencies.** `Subtask` ([conduct.rs:16-24](../../core/src/orchestrator/conduct.rs)) carries only `depends_note: String` — a free-text hint that is **parsed and never read** (grep: the only references are the struct definition and the test constructor). The decompose prompt ([prompts.rs:71-93](../../core/src/orchestrator/prompts.rs)) tells the conductor to "design subtasks so they touch DISJOINT files; they run sequentially" — so ordering is implicit and unenforced. If the conductor lists a dependent subtask before its prerequisite, it runs first.

- **Any subtask failure aborts the entire run.** Every terminal-failure site does `break 'subtask`, which breaks the `for subtask in &plan.subtasks` loop — stopping *all* remaining subtasks, including ones that don't depend on the failure. The sites:
  - worker reworks exhausted ([conduct.rs:398-411](../../core/src/orchestrator/conduct.rs))
  - conductor `EvalDecision::Fail` ([conduct.rs:705-718](../../core/src/orchestrator/conduct.rs))
  - rework exhausted / stalled ([conduct.rs:719-746](../../core/src/orchestrator/conduct.rs))
  - review/arbiter `GateDecision::Fail` ([conduct.rs:683-702](../../core/src/orchestrator/conduct.rs))

  (Supervisor `Halt` at [conduct.rs:494-504](../../core/src/orchestrator/conduct.rs) and budget-exceeded at [conduct.rs:314-325](../../core/src/orchestrator/conduct.rs) are **intentionally global** aborts — they stay.)

A DAG with `skip-failed-dependency` is only meaningful if a failed subtask does **not** halt independent work — so slice 2 necessarily changes the failure model from "fail-stops-all" to "fail-isolates-to-dependents." That is the central behavior change of this plan; it is called out again in the self-review.

## Key design decisions

1. **`depends_on` is additive; `depends_note` stays.** New field `depends_on: Vec<u32>` with `#[serde(default)]` — old plans (and the replan path) parse unchanged with an empty edge set, which means **wave 0 = all subtasks in original order = today's behavior exactly.** `depends_note` (prose) is retained as worker-facing context; we don't disturb its contract.

2. **Layering is a pure, isolated module.** `topology::plan_waves(&[Subtask]) -> anyhow::Result<Vec<Vec<usize>>>` returns waves of **indices** into the slice (no cl, no borrow of `Subtask` internals beyond `id`/`depends_on`). Pure ⇒ exhaustively unit-testable (cycle, self-edge, unknown id, duplicate id, diamond). `run_conduct` calls it; everything else is mechanical.

3. **Failure isolates; Halt/budget stay global.** Subtask-failure sites change from `break 'subtask` to `continue 'next_subtask` (record the failure, keep the first failure as the run-level `failed` headline, move on). A subtask whose `depends_on` is not fully ⊆ `completed` is recorded `skipped` and skipped — which transitively skips its own dependents (a skipped subtask never enters `completed`). The existing replan gate (`failed.is_some()` after the plan loop) is unchanged; it simply now fires after independent work has run, giving the replan more `completed` context.

4. **Execution stays sequential.** Waves are iterated, and **within a wave subtasks still run one at a time** in the shared `cwd`. This proves DAG ordering + failure isolation with zero concurrency risk and is the literal structure Phase B will parallelize (swap the inner per-wave `for` for a `join_all`). No worktrees, no protocol changes, no streaming changes in this plan.

## File structure

| File | Change | Responsibility |
|------|--------|----------------|
| `core/src/orchestrator/conduct.rs` | Modify | Add `depends_on` to `Subtask`; wave-ordered + failure-isolating loop; `skipped` in transcript; update `st()` helper + tests. |
| `core/src/orchestrator/topology.rs` | **Create** | Pure `plan_waves` (Kahn layering) + cycle/edge validation + unit tests. |
| `core/src/orchestrator/mod.rs` | Modify | `pub mod topology;`. |
| `core/src/orchestrator/prompts.rs` | Modify | `conduct_decompose` + `conduct_replan` emit `depends_on`; update template + drift-guard tests. |

No call-site changes: `run_conduct`'s signature is untouched (callers at [server.rs:181](../../core/src/server.rs), [auto.rs:107](../../core/src/orchestrator/auto.rs), [main.rs:448](../../core/src/main.rs), [eval.rs:399](../../core/src/orchestrator/eval.rs) compile as-is).

---

## Phase A — Tasks

### Task 1: Add the `depends_on` edge to `Subtask`

**Files:**
- Modify: `core/src/orchestrator/conduct.rs:16-24` (struct), `:1351-1358` (`st()` test helper)
- Test: `core/src/orchestrator/conduct.rs` (existing `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test** — add to `conduct.rs`'s test module:

```rust
#[test]
fn subtask_depends_on_defaults_empty_and_parses() {
    // Back-compat: a plan with NO depends_on parses with an empty edge set.
    let old = parse_plan(r#"{"subtasks":[{"id":1,"title":"t","prompt":"p","depends_note":""}]}"#)
        .unwrap();
    assert!(old.subtasks[0].depends_on.is_empty());

    // New: an explicit edge list parses.
    let new = parse_plan(
        r#"{"subtasks":[{"id":2,"title":"t","prompt":"p","depends_on":[1]}]}"#,
    )
    .unwrap();
    assert_eq!(new.subtasks[0].depends_on, vec![1]);
}
```

- [ ] **Step 2: Run it — verify it fails to compile**

Run: `cargo test -p consilium subtask_depends_on_defaults_empty_and_parses`
Expected: FAIL — `no field 'depends_on' on type 'Subtask'`.

- [ ] **Step 3: Add the field + fix the test constructor**

In `conduct.rs:16-24`, add the field (keep `depends_note`):

```rust
#[derive(Debug, Deserialize)]
pub struct Subtask {
    pub id: u32,
    #[serde(default)]
    pub title: String,
    pub prompt: String,
    #[serde(default)]
    pub depends_note: String,
    /// Ids of subtasks that must COMPLETE before this one runs. `#[serde(default)]`
    /// ⇒ a plan that omits it parses as no-edges (today's behavior). Validated +
    /// layered by `crate::orchestrator::topology::plan_waves`.
    #[serde(default)]
    pub depends_on: Vec<u32>,
}
```

In the `st()` test helper (`conduct.rs:1351-1358`), add the field so existing tests compile:

```rust
fn st(id: u32, title: &str, prompt: &str) -> Subtask {
    Subtask {
        id,
        title: title.into(),
        prompt: prompt.into(),
        depends_note: String::new(),
        depends_on: Vec::new(),
    }
}
```

- [ ] **Step 4: Run the test — verify it passes**

Run: `cargo test -p consilium subtask_depends_on`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add core/src/orchestrator/conduct.rs
git commit -m "feat(conduct): add depends_on edge to Subtask (serde-default, back-compat)"
```

---

### Task 2: Pure topological layering module

**Files:**
- Create: `core/src/orchestrator/topology.rs`
- Modify: `core/src/orchestrator/mod.rs`

- [ ] **Step 1: Declare the module**

In `core/src/orchestrator/mod.rs`, add (alphabetical, after `pub mod stagnation;`):

```rust
pub mod topology;
```

- [ ] **Step 2: Write the failing tests** — create `core/src/orchestrator/topology.rs` with ONLY the tests first (and a stub signature so it compiles):

```rust
//! Pure dependency-layering for a conduct plan: groups subtasks into waves where
//! every subtask in wave N depends only on subtasks in waves < N (Kahn's
//! algorithm). Sequential execution iterates the waves in order; Phase B runs a
//! wave's members concurrently. No I/O, no engine state — exhaustively unit-tested.

use crate::orchestrator::conduct::Subtask;

/// Group subtasks into dependency waves, returned as vectors of INDICES into
/// `subtasks`. Within a wave, original slice order is preserved (deterministic).
///
/// Errors (the conductor produced an invalid DAG):
/// - a `depends_on` id that no subtask defines,
/// - a self-edge,
/// - duplicate subtask ids (ambiguous edges),
/// - a cycle (no subtask becomes ready while work remains).
pub fn plan_waves(_subtasks: &[Subtask]) -> anyhow::Result<Vec<Vec<usize>>> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::conduct::Subtask;

    fn sub(id: u32, deps: &[u32]) -> Subtask {
        Subtask {
            id,
            title: String::new(),
            prompt: String::new(),
            depends_note: String::new(),
            depends_on: deps.to_vec(),
        }
    }

    #[test]
    fn no_deps_is_one_wave_in_original_order() {
        let s = vec![sub(1, &[]), sub(2, &[]), sub(3, &[])];
        let waves = plan_waves(&s).unwrap();
        assert_eq!(waves, vec![vec![0, 1, 2]], "empty edges ⇒ today's order");
    }

    #[test]
    fn linear_chain_is_one_per_wave() {
        // 1 → 2 → 3, declared out of order to prove layering, not slice order, wins.
        let s = vec![sub(3, &[2]), sub(1, &[]), sub(2, &[1])];
        let waves = plan_waves(&s).unwrap();
        // indices: 1 is at idx 1, 2 at idx 2, 3 at idx 0.
        assert_eq!(waves, vec![vec![1], vec![2], vec![0]]);
    }

    #[test]
    fn diamond_groups_the_middle_pair() {
        // 1 → {2,3} → 4
        let s = vec![sub(1, &[]), sub(2, &[1]), sub(3, &[1]), sub(4, &[2, 3])];
        let waves = plan_waves(&s).unwrap();
        assert_eq!(waves, vec![vec![0], vec![1, 2], vec![3]]);
    }

    #[test]
    fn unknown_dependency_is_an_error() {
        let s = vec![sub(1, &[9])];
        let err = plan_waves(&s).unwrap_err().to_string();
        assert!(err.contains("unknown"), "got: {err}");
    }

    #[test]
    fn self_edge_is_an_error() {
        let s = vec![sub(1, &[1])];
        assert!(plan_waves(&s).unwrap_err().to_string().contains("itself"));
    }

    #[test]
    fn cycle_is_an_error() {
        let s = vec![sub(1, &[2]), sub(2, &[1])];
        assert!(plan_waves(&s).unwrap_err().to_string().contains("cycle"));
    }

    #[test]
    fn duplicate_ids_are_an_error() {
        let s = vec![sub(1, &[]), sub(1, &[])];
        assert!(plan_waves(&s).unwrap_err().to_string().contains("duplicate"));
    }

    #[test]
    fn empty_plan_is_no_waves() {
        assert_eq!(plan_waves(&[]).unwrap(), Vec::<Vec<usize>>::new());
    }
}
```

- [ ] **Step 3: Run the tests — verify they fail**

Run: `cargo test -p consilium topology::`
Expected: FAIL — `not implemented` panic at runtime in each test.

- [ ] **Step 4: Implement `plan_waves`** — replace the stub body:

```rust
pub fn plan_waves(subtasks: &[Subtask]) -> anyhow::Result<Vec<Vec<usize>>> {
    use std::collections::HashSet;

    // Unique-id check: edges reference ids, so duplicate ids make edges ambiguous.
    let mut ids: HashSet<u32> = HashSet::new();
    for s in subtasks {
        if !ids.insert(s.id) {
            anyhow::bail!("plan has duplicate subtask id {}", s.id);
        }
    }

    // Edge validation: every dep must reference a known, non-self id.
    for s in subtasks {
        for d in &s.depends_on {
            if *d == s.id {
                anyhow::bail!("subtask {} depends on itself", s.id);
            }
            if !ids.contains(d) {
                anyhow::bail!("subtask {} depends on unknown subtask {}", s.id, d);
            }
        }
    }

    // Kahn layering: each round, a wave = every not-yet-placed subtask whose deps
    // are all already placed. O(n²) — n ≤ 5 in practice.
    let mut placed: HashSet<u32> = HashSet::new();
    let mut done = vec![false; subtasks.len()];
    let mut waves: Vec<Vec<usize>> = Vec::new();

    while placed.len() < subtasks.len() {
        let wave: Vec<usize> = subtasks
            .iter()
            .enumerate()
            .filter(|(i, s)| !done[*i] && s.depends_on.iter().all(|d| placed.contains(d)))
            .map(|(i, _)| i)
            .collect();
        if wave.is_empty() {
            anyhow::bail!("dependency cycle among subtasks (no subtask is runnable)");
        }
        for &i in &wave {
            done[i] = true;
            placed.insert(subtasks[i].id);
        }
        waves.push(wave);
    }
    Ok(waves)
}
```

- [ ] **Step 5: Run the tests — verify they pass**

Run: `cargo test -p consilium topology::`
Expected: PASS (8 tests).

- [ ] **Step 6: Commit**

```bash
git add core/src/orchestrator/topology.rs core/src/orchestrator/mod.rs
git commit -m "feat(conduct): pure plan_waves topological layering + tests"
```

---

### Task 3: Conductor emits `depends_on` in plans

**Files:**
- Modify: `core/src/orchestrator/prompts.rs:71-93` (`conduct_decompose`), `:95-120` (`conduct_replan`)
- Test: `core/src/orchestrator/conduct.rs` test module

- [ ] **Step 1: Write the failing test** — add to `conduct.rs`'s test module:

```rust
#[test]
fn decompose_template_emits_depends_on_edge() {
    // The few-shot example must teach the edge: subtask 2 depends on subtask 1.
    let p = crate::orchestrator::prompts::conduct_decompose("t", "ctx");
    let plan = parse_plan(&p).expect("decompose template example must parse");
    let s2 = plan.subtasks.iter().find(|s| s.id == 2).expect("example has subtask 2");
    assert_eq!(s2.depends_on, vec![1], "example must show a real depends_on edge");
}
```

- [ ] **Step 2: Run it — verify it fails**

Run: `cargo test -p consilium decompose_template_emits_depends_on_edge`
Expected: FAIL — `s2.depends_on` is `[]` (the current template has no `depends_on`).

- [ ] **Step 3: Update the prompts.** In `conduct_decompose` ([prompts.rs:71-93](../../core/src/orchestrator/prompts.rs)), (a) add an instruction sentence after the "touch DISJOINT files" line, (b) add `depends_on` to both example subtasks, (c) add it to the trailing output-shape line. Replace the disjoint-files sentence and the two JSON shapes:

Change the sentence
```
Design subtasks so they touch DISJOINT files; they run sequentially.\n\n\
```
to
```
Design subtasks so they touch DISJOINT files. Express ordering explicitly: each \
subtask has a `depends_on` array listing the ids of subtasks that must finish \
first (empty for independent subtasks). Independent subtasks may run together; a \
subtask whose dependency fails is skipped, so only add an edge when the work \
genuinely needs the earlier result.\n\n\
```

In the **example** block, give subtask 1 `"depends_on":[]` and subtask 2 `"depends_on":[1]` (keep `depends_note`):
```
```json\n{{\"subtasks\":[{{\"id\":1,\"title\":\"retry helper\",\"prompt\":\"In src/util/retry.rs add `pub async fn with_backoff<F,T>(max: u32, base: Duration, f: F) -> anyhow::Result<T>` that retries f up to max times, sleeping base*2^attempt between tries and returning the last error on exhaustion. Add #[tokio::test]s for success-after-one-failure and exhaustion. Touch only this file.\",\"depends_note\":\"\",\"depends_on\":[]}},{{\"id\":2,\"title\":\"wire --max-retries\",\"prompt\":\"In src/main.rs add a clap flag --max-retries <u32> (default 3) to the run subcommand and pass it into with_backoff; leave other flags unchanged.\",\"depends_note\":\"uses retry helper from subtask 1\",\"depends_on\":[1]}}]}}\n```
```

In the trailing shape line, add the field:
```
```json\n{{\"subtasks\":[{{\"id\":1,\"title\":\"short name\",\"prompt\":\"full self-contained instructions\",\"depends_note\":\"\",\"depends_on\":[]}}]}}\n```
```

In `conduct_replan` ([prompts.rs:95-120](../../core/src/orchestrator/prompts.rs)), change its disjoint-files sentence and trailing shape the same way. Replace
```
Design subtasks so they touch DISJOINT files; they run sequentially.\n\n\
```
with
```
Design subtasks so they touch DISJOINT files, and give each a `depends_on` array \
of the ids it requires (use ids from the completed work above when a new subtask \
builds on finished work; empty for independent subtasks).\n\n\
```
and the trailing shape
```
```json\n{{\"subtasks\":[{{\"id\":1,\"title\":\"short name\",\"prompt\":\"full self-contained instructions\",\"depends_note\":\"\"}}]}}\n```
```
to
```
```json\n{{\"subtasks\":[{{\"id\":1,\"title\":\"short name\",\"prompt\":\"full self-contained instructions\",\"depends_note\":\"\",\"depends_on\":[]}}]}}\n```
```

- [ ] **Step 4: Run the test + the existing template guards — verify all pass**

Run: `cargo test -p consilium decompose_template_emits_depends_on_edge decompose_template_example_parses_as_plan`
Expected: PASS (the new edge test, and the existing `decompose_template_example_parses_as_plan` still parses).

- [ ] **Step 5: Commit**

```bash
git add core/src/orchestrator/prompts.rs core/src/orchestrator/conduct.rs
git commit -m "feat(conduct): decompose/replan prompts emit depends_on edges"
```

---

### Task 4: Execute in wave order (still sequential)

This restructures `run_conduct`'s subtask loop from `for subtask in &plan.subtasks` to nested wave iteration, preserving every existing arm. Mechanical relabel: `'subtask` → two labels (`'plan` outer, `'next_subtask` inner). **No failure-model change yet** — that is Task 5; here a failure still ends the plan (`break 'plan`), so behavior with real plans is identical except ordering now respects `depends_on`.

**Files:**
- Modify: `core/src/orchestrator/conduct.rs` — loop header (`:312-313`), the `for subtask` line, and every `*'subtask` label inside.
- Test: `core/src/orchestrator/conduct.rs` test module + the existing scripted integration tests (run them to prove no regression).

- [ ] **Step 1: Write the failing test** (a pure ordering assertion via `plan_waves`, plus a guard the loop uses it):

```rust
#[test]
fn waves_order_respects_depends_on_even_when_listed_out_of_order() {
    use crate::orchestrator::topology::plan_waves;
    // Conductor lists the dependent FIRST; layering must still run it last.
    let plan = vec![
        st_dep(2, "second", "p2", &[1]),
        st_dep(1, "first", "p1", &[]),
    ];
    let waves = plan_waves(&plan).unwrap();
    let flat: Vec<u32> = waves.iter().flatten().map(|&i| plan[i].id).collect();
    assert_eq!(flat, vec![1, 2], "dependency runs before dependent");
}
```

Add a second test-helper next to `st()` in the test module:

```rust
fn st_dep(id: u32, title: &str, prompt: &str, deps: &[u32]) -> Subtask {
    Subtask { depends_on: deps.to_vec(), ..st(id, title, prompt) }
}
```

- [ ] **Step 2: Run it — verify it fails to compile**

Run: `cargo test -p consilium waves_order_respects_depends_on`
Expected: FAIL — `st_dep` undefined until added (then the assertion itself passes once `st_dep` compiles, since `plan_waves` already exists). This test mainly pins the contract the loop relies on.

- [ ] **Step 3: Restructure the loop.** In `run_conduct`, immediately before the `'subtask:` loop (currently [conduct.rs:312-313](../../core/src/orchestrator/conduct.rs)), compute the waves from the current plan and replace the loop header. Change:

```rust
    loop {
        'subtask: for subtask in &plan.subtasks {
```

to:

```rust
    loop {
        // DAG layering: iterate dependency waves, not raw plan order. An
        // unlayerable plan (cycle / bad edge from the conductor) fails the run
        // cleanly rather than panicking.
        let waves = match crate::orchestrator::topology::plan_waves(&plan.subtasks) {
            Ok(w) => w,
            Err(e) => {
                failed = Some(format!("invalid plan: {e}"));
                break;
            }
        };
        'plan: for wave in &waves {
            'next_subtask: for &subtask_idx in wave {
                let subtask = &plan.subtasks[subtask_idx];
```

Then, inside the body:
- Every `continue 'subtask;` (the accept paths at [conduct.rs:655](../../core/src/orchestrator/conduct.rs) and [:670](../../core/src/orchestrator/conduct.rs)) → `continue 'next_subtask;`.
- Every `break 'subtask;` that is a **subtask failure** (worker exhausted [:410](../../core/src/orchestrator/conduct.rs); `GateDecision::Fail` [:701](../../core/src/orchestrator/conduct.rs); `EvalDecision::Fail` [:717](../../core/src/orchestrator/conduct.rs); rework exhausted/stalled [:745](../../core/src/orchestrator/conduct.rs)) → `break 'plan;` **for now** (Task 5 converts these to `continue 'next_subtask`).
- The supervisor **Halt** `break 'subtask;` ([:503](../../core/src/orchestrator/conduct.rs)) → `break 'plan;` (stays a global abort).
- The budget-exceeded `break;` at [conduct.rs:323](../../core/src/orchestrator/conduct.rs) → `break 'plan;`.
- Close the two new `for` blocks (`'next_subtask` and `'plan`) with extra braces before the budget/replan tail at [conduct.rs:760](../../core/src/orchestrator/conduct.rs).

The `for attempt_num in 0..=(MAX_REWORKS as usize)` loop and its inner bare `continue;` (retry, [:421](../../core/src/orchestrator/conduct.rs)) and rework-prepare fall-through are unchanged.

- [ ] **Step 4: Run the focused test + the full scripted conduct suite — verify no regression**

Run: `cargo test -p consilium conduct`
Expected: PASS — the new ordering test, plus all existing `run_conduct` scripted integration tests (they use empty `depends_on` ⇒ single wave ⇒ identical order/behavior).

- [ ] **Step 5: Commit**

```bash
git add core/src/orchestrator/conduct.rs
git commit -m "feat(conduct): execute subtasks in dependency-wave order (sequential)"
```

---

### Task 5: Failure isolation + skip-failed-dependency

Convert subtask-failure aborts into isolation: a failed subtask records its failure and moves on; a subtask whose prerequisites are not all in `completed` is recorded `skipped` (which transitively skips its dependents). Halt/budget remain global.

**Files:**
- Modify: `core/src/orchestrator/conduct.rs` — add `skipped` accumulator; skip-guard at subtask entry; convert the four subtask-failure `break 'plan` → `continue 'next_subtask`; transcript.
- Test: `core/src/orchestrator/conduct.rs` test module (scripted integration tests).

- [ ] **Step 1: Write the failing tests.** These use the existing scripted-adapter conduct test harness. Mirror an existing scripted `run_conduct` test (find one in the test module, e.g. a worker-failure case) for the exact builder; the two new cases assert:

```rust
// (A) An independent subtask still completes after another subtask fails.
#[tokio::test]
async fn independent_subtask_completes_despite_a_sibling_failure() {
    // Plan: subtask 1 (worker always fails) and subtask 2 (depends_on []),
    // both in wave 0. Expect: completed contains 2; failed is Some; the run is
    // NOT aborted before 2 runs.
    // ...build scripted deps mirroring the existing worker-failure test...
    let outcome = /* run_conduct(...) */;
    assert!(outcome.completed.contains(&2), "independent work runs despite sibling failure");
    assert!(outcome.failed.is_some(), "the run still reports the failure");
}

// (B) A dependent subtask is SKIPPED when its prerequisite fails.
#[tokio::test]
async fn dependent_subtask_is_skipped_when_prerequisite_fails() {
    // Plan: subtask 1 (worker always fails); subtask 2 depends_on [1].
    // Expect: 2 is neither completed nor attempted — recorded skipped; the
    // transcript lists 2 under "skipped".
    let outcome = /* run_conduct(...) */;
    assert!(!outcome.completed.contains(&2));
    let skipped = outcome.transcript["skipped"].as_array().unwrap();
    assert!(skipped.iter().any(|v| v.as_u64() == Some(2)), "dependent is skipped");
}
```

> Implementer note: copy the scripted-deps construction verbatim from the nearest existing `#[tokio::test]` that drives `run_conduct` with a failing worker; only the plan's `depends_on` and the assertions differ. Do not invent a new harness.

- [ ] **Step 2: Run them — verify they fail**

Run: `cargo test -p consilium independent_subtask_completes dependent_subtask_is_skipped`
Expected: FAIL — today a failure does `break 'plan` (so subtask 2 never runs and is not skipped-recorded; `transcript["skipped"]` is absent).

- [ ] **Step 3a: Add the `skipped` accumulator.** Next to `let mut completed: Vec<u32> = Vec::new();` ([conduct.rs:284](../../core/src/orchestrator/conduct.rs)), add:

```rust
    let mut skipped: Vec<u32> = Vec::new();
```

- [ ] **Step 3b: Add the skip-guard** at the top of the `'next_subtask` body, immediately after `let subtask = &plan.subtasks[subtask_idx];` (from Task 4):

```rust
                // DAG failure isolation: a subtask whose prerequisites did not
                // COMPLETE (failed, skipped, or — impossible under layering — not
                // yet run) is skipped, not attempted. Because a skipped subtask
                // never enters `completed`, this transitively skips its dependents.
                let unmet: Vec<u32> = subtask
                    .depends_on
                    .iter()
                    .copied()
                    .filter(|d| !completed.contains(d))
                    .collect();
                if !unmet.is_empty() {
                    skipped.push(subtask.id);
                    subtask_entries.push(build_subtask_entry(
                        subtask.id,
                        &subtask.title,
                        "skipped",
                        &[],
                        &[],
                    ));
                    continue 'next_subtask;
                }
```

- [ ] **Step 3c: Convert the four subtask-failure sites** from `break 'plan;` (set in Task 4) to isolation. At each of the worker-exhausted, `EvalDecision::Fail`, rework-exhausted/stalled, and `GateDecision::Fail` sites, change the pattern

```rust
                            failed = Some(format!("subtask {} ...", subtask.id, ...));
                            subtask_entries.push(build_subtask_entry(...));
                            break 'plan;
```

to keep only the **first** failure as the run headline and continue:

```rust
                            if failed.is_none() {
                                failed = Some(format!("subtask {} ...", subtask.id, ...));
                            }
                            subtask_entries.push(build_subtask_entry(...));
                            continue 'next_subtask;
```

Apply to all four failure sites. **Do NOT touch** the supervisor `Halt` site (keep `break 'plan;`) or the budget-exceeded `break 'plan;`.

> Note: the `GateDecision::Fail` site embeds an arbiter entry before pushing — preserve that block; only the `failed` guard + `break 'plan` → `continue 'next_subtask` change.

- [ ] **Step 3d: Record `skipped` in the transcript.** In the `serde_json::json!` transcript ([conduct.rs:831-846](../../core/src/orchestrator/conduct.rs)), add a field after `"completed": completed,`:

```rust
        "skipped": skipped,
```

- [ ] **Step 4: Run the new tests + full conduct suite — verify all pass**

Run: `cargo test -p consilium conduct`
Expected: PASS — the two new isolation tests, and every existing conduct test (single-wave, empty-deps plans now hit `continue 'next_subtask` at the end of the wave instead of `break`, but since the wave is exhausted the outcome is identical: same `completed`, same `failed`).

- [ ] **Step 5: Commit**

```bash
git add core/src/orchestrator/conduct.rs
git commit -m "feat(conduct): isolate subtask failures; skip unmet-dependency subtasks"
```

---

### Task 6: Gate, dogfood, docs & memory

**Files:**
- Modify: `README.md` (conduct description — note DAG + failure isolation), `/Users/temur/.claude/projects/-Users-temur-Desktop-Claude/memory/project_consilium.md` (mark slices 1+2 done, Phase B pending).

- [ ] **Step 1: Full gate — must be green before merge**

Run:
```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test -p consilium
```
Expected: all PASS, zero warnings.

- [ ] **Step 2: Real dogfood smoke** — a tiny two-subtask DAG against a scratch repo, confirming (a) dependency order and (b) a forced prerequisite failure skips the dependent. Use a throwaway `cwd` (e.g. `/tmp/consilium-fanout-smoke`, `git init`) so nothing in the real tree is touched.

Run: `consilium conduct "Add a hello() to src/a.rs, then a test in src/b.rs that calls it" --cwd /tmp/consilium-fanout-smoke` (or the repo's existing conduct invocation).
Expected: transcript shows subtask 2 (test) ordered after subtask 1, and on an injected failure of subtask 1, subtask 2 appears under `skipped`.

- [ ] **Step 3: Update README + memory** — one paragraph in the conduct section noting subtasks now form a `depends_on` DAG executed in dependency waves, with failure isolation (a failed subtask skips only its dependents). In `project_consilium.md`, mark fan-out slices 1+2 done and Phase B (worktree/parallel/streaming) as the deferred follow-up needing its own brainstorm.

- [ ] **Step 4: Commit**

```bash
git add README.md
git commit -m "docs(conduct): document the subtask DAG + failure isolation"
```

- [ ] **Step 5: Merge** (per the repo's branch → gate → `merge --no-ff` → push workflow).

---

## Phase B — deferred (worktree isolation + parallel execution + streaming attribution)

Slices 3-5 deliver **true concurrency**: a wave's members run at the same time. They are deferred to their own brainstorm + plan because the design is **not settled** — writing TDD steps now would mean hand-waving real git plumbing (which the writing-plans skill forbids). The open questions:

1. **Worktree isolation & the inheritance model (slice 3).** Parallel workers cannot share one `cwd` — `capture_changes` (`git diff HEAD`) couldn't attribute a diff to a subtask, and `verify` (`cargo build`) would race a concurrent writer. So each worker needs its own checkout. But [conduct.rs:306-311](../../core/src/orchestrator/conduct.rs) already records the tension: worktrees branch from a base, so they **lose the live-tree inheritance** the worker blackboard relies on (a later worker today sees earlier workers' uncommitted edits). The fork: how to snapshot the accumulated dirty tree — *including untracked files* — into an isolated base each wave (`git stash create` excludes untracked; a temp index + `write-tree` includes them but is fiddly), and how to merge disjoint per-subtask diffs back into `cwd` (apply patch vs ephemeral commit). Each option changes verify semantics (isolated-against-base vs cumulative). **Needs a design decision before any code.**

2. **Concurrency mechanics (slice 4).** `join_all` is the right primitive (council.rs already fans out members this way, and it preserves the task-local sink — no `tokio::spawn`). The per-wave body must move into a closure capturing per-subtask state; today the loop body mutates shared `subtask_entries` / `completed` / `all_fallbacks` inline, so those must become per-subtask results merged **deterministically by id** after the wave's barrier. The supervisor/replan stages assume a single in-flight subtask — they need a defined order relative to a wave.

3. **Streaming attribution (slice 5).** Under `join_all`, all members share one task and one `PROGRESS_SINK`, so events interleave with **no member attribution**. Fix without `tokio::spawn`: wrap each member future in `PROGRESS_SINK.scope(member_tagged_sink, fut)` — `scope` is per-future, so each poll activates the right member's sink even on a shared task. That implies a sink wrapper carrying a `subtask_id`/`member_label`, and a wire change to tag streamed events (new optional field on the envelope or a `ProgressSink` method) — a `ts-rs` protocol change the UI must consume. **Needs the protocol shape decided.**

**Recommendation:** ship Phase A (this plan), then run a brainstorming pass on Phase B starting from question 1 (it gates the rest).

---

## Self-review

**1. Spec coverage.** Memory slices 1 (depends_on + layering) and 2 (skip-failed-dependency) are covered by Tasks 1-5; slices 3-5 are explicitly deferred with rationale (not silently dropped). ✓

**2. Behavior-change callout (must surface at review).** Task 5 changes conduct's failure model: a subtask failure **no longer aborts the run** — independent subtasks still run; only dependents skip. This is inherent to "skip-failed-dependency" but is a real semantic shift in a load-bearing loop. The first failure is preserved as the run-level `failed` headline (so the run still reports failure and still triggers replan); per-subtask entries carry each failure. Reviewer should confirm this matches intent. ✓ (flagged)

**3. Placeholder scan.** One intentional soft spot: Task 5's tests say "mirror the nearest existing scripted `run_conduct` test" rather than reproducing the scripted-adapter builder, because that harness wasn't read in full during planning. This is a pointer to a concrete in-repo pattern, not a vague "write tests" — but the implementer must open the test module and copy the real builder. Everything else (struct, `plan_waves`, prompt edits, loop relabel, skip-guard, transcript) is complete code. ✓

**4. Type/label consistency.** `depends_on: Vec<u32>` used consistently (struct, `st_dep`, `plan_waves`, skip-guard). Loop labels: `'subtask` → `'plan` (outer wave) + `'next_subtask` (inner subtask), applied in Task 4 and referenced consistently in Task 5. `plan_waves` returns `Vec<Vec<usize>>` (indices) and every consumer indexes `plan.subtasks[idx]`. `build_subtask_entry(id, title, status, attempts, supervisor)` called with `&[]`/`&[]` for the skip entry — matches its signature ([conduct.rs:1182](../../core/src/orchestrator/conduct.rs)). ✓

**5. No call-site churn.** `run_conduct`'s signature is unchanged; `ConductOutcome` gains nothing (skipped lives in the transcript JSON only), so `ServerFrame`/`ts-rs` are untouched — no protocol regeneration in Phase A. ✓
