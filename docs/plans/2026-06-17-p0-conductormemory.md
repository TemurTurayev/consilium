# P0 #2 — ConductorMemory: a live plan ledger + cumulative attempt history

> Status: APPROVED — implementing. Second slice of the harness-leveling P0 trio
> (`docs/research/2026-06-17-harness-leveling-research.md`). Slice 1 (build/test
> grounding) is merged. Slice 3 (worker blackboard + worktree isolation) is a
> separate plan. **This plan was adversarially reviewed across three lenses
> (architecture/keystone, test-seams, cost/security/scope); the v2 below folds in
> all five required changes. Req. change #5 (ship dark / default-off) is
> deliberately overridden by an explicit owner decision: default ON — the
> mechanism still elides empty blocks, so cost is paid only where prior context
> exists, and dogfood measures the impact.**

## Problem (confirmed against the code, not assumed)

An understand-phase fan-out + an adversarial premise audit confirmed: **within a
single `conduct` run the conductor is fully stateless.** Every conductor-facing
stage is rebuilt from current-attempt data only:

- `conduct_evaluation` (conduct.rs:427-433) sees only *this* attempt's changes /
  worker report / verify summary. It does **not** see prior rejection rounds for
  the same subtask, so it can re-issue the same feedback and oscillate.
- `conduct_rework` (conduct.rs:348, 635, 680) threads only the **latest** single
  feedback string — prior rounds' feedback is dropped. The reworking attempt
  cannot tell what was already tried and rejected.
- `supervisor_gate` (conduct.rs:367) — whose literal job is to flag "repeated
  failures" (prompts.rs:103) — carries no per-subtask attempt history, only a
  bare `completed: Vec<u32>` of IDs and a 500-char diff preview
  (conduct.rs:724-729).
- `arbiter_decide` (conduct.rs:557-560) carries no rework history that led to it.
- The on-disk transcript is **write-only** — assembled once at conduct.rs:697-706,
  never read back into any prompt. No adapter passes `--resume`/`--session-id`
  (audit: zero matches); every stage spawns a fresh process.

Research is explicit that an agent loop without carried-forward memory of its own
prior judgments degrades on long-horizon, multi-round work. This slice gives the
conductor a working memory that travels as prompt text — keeping the
stateless-process architecture intact (no adapter / sessions.rs changes).

## Scope boundary (P0 #2 vs P0 #3 — restated per review)

**This slice adds cross-subtask CONDUCTOR context only** (a plan ledger injected
into the conductor-facing roles: evaluation, supervisor, arbiter) **plus this
subtask's own cumulative attempt history.** Workers remain fully isolated per
subtask: `conduct_rework` threads **only this subtask's own `attempt_history`,
never the ledger and never other subtasks' artifacts.** Cross-subtask WORKER
visibility + git-worktree isolation stay in **P0 #3 (worker blackboard)**.
`conduct_decompose` is unchanged (runs before any memory exists; we do not
re-decompose today).

## Design (v2)

### Single source of truth: enrich `subtask_entries`, don't shadow it

The architecture review caught that the loop has **eight** terminal
`subtask_entries.push` sites (not five), and any parallel ledger `Vec` risks
drifting or omitting one (it already omitted site 624). Fix at the root: **route
all eight pushes through one helper** and render the ledger from
`subtask_entries` itself.

```rust
// new in conduct.rs — the ONLY place a finished subtask entry is built
fn build_subtask_entry(
    id: u32, title: &str, status: &str,           // "completed" | "failed" | "halted"
    attempts: &[serde_json::Value],
    supervisor: &[serde_json::Value],
) -> serde_json::Value {
    // summary is MECHANICAL only (req. change #4): no worker/feedback text.
    let verify_digest = last_verify(attempts);     // "passed" | "failed" | "not_run" | "-"
    let summary = format!("{status} (verify: {verify_digest})");
    serde_json::json!({
        "id": id, "title": title, "status": status, "summary": summary,
        "attempts": attempts, "supervisor": supervisor,
    })
}
```

The eight call-sites, each mapped to a status (verified against the code):

| line | branch | status |
|---|---|---|
| 338 | worker-fail + reworks exhausted | `failed` |
| 400 | supervisor Halt | `halted` |
| 589 | arbiter Ship | `completed` (+ `arbiter` field inserted after) |
| 605 | arbiter Fail | `failed` (+ `arbiter` field) |
| 624 | review-gate exhausted, no arbiter | `failed` |
| 642 | accept (review clean / no reviewer) | `completed` |
| 655 | conductor eval Fail | `failed` |
| 670 | conductor rework exhausted | `failed` |

The two arbiter sites build via the helper, then
`obj.insert("arbiter", …)` before push (same `as_object_mut` idiom already used
for the `review` field at conduct.rs:543-549). Adding `status`/`summary` to every
entry also enriches the transcript for free.

### Ledger rendered from prior `subtask_entries`

While processing subtask *k*, `subtask_entries` holds **exactly** subtasks
`1..k-1` (the current one isn't pushed until it terminates). So:

```rust
fn render_ledger(prior_entries: &[serde_json::Value], cap: usize) -> Option<String>
fn render_attempt_history(attempts: &[serde_json::Value], cap: usize) -> Option<String>
```

- Both return **`None`** when there's nothing to show (no prior subtasks; attempt
  0 has no history). `None` → the prompt omits the block entirely (see threading).
- **Content contract (req. change #4):**
  - `render_ledger` emits per prior subtask only `id`, `title`, `status`,
    `summary` (the mechanical digest) — **zero** verbatim worker/feedback text.
  - `render_attempt_history` emits per prior round `{attempt, decision, verify,
    feedback}`. Feedback is the conductor's own text and MAY appear here (that is
    the whole point — so it stops repeating itself), XML-isolated. Feedback never
    leaks into `ledger.summary`.
- **Caps (req. change #3):** two **char** budgets (not tokens — consistent with
  `verify::TAIL_CAP = 3000`, verify.rs:15; these are smaller):
  `ledger_char_cap` default **1500**, `attempt_history_char_cap` default **800**.
  Budget is applied **per rendered block, per call**. When over budget, keep the
  **most-recent** records and prepend `"(… N earlier elided)"` — **the marker is
  counted inside the budget** (so it can't itself be an injection vector). Note:
  `attempt_history` is re-rendered into *each* rework call and the evaluation
  call; the cap is per-call, so worst case per subtask is bounded by
  `attempt_history_char_cap × (MAX_REWORKS rework calls + 1 eval)`.

### Prompt threading (prompts.rs — `Option<&str>`, supervisor_note idiom)

New params are `Option<&str>` and rendered with the existing
`.map(|s| format!("\n<tag>\n{s}\n</tag>\n")).unwrap_or_default()` pattern
(prompts.rs:72-74). `None` ⇒ block fully omitted ⇒ first-attempt / single-subtask
/ memory-disabled prompts are **byte-identical** to today (req. change #2).

| stage (role) | `plan_ledger` | `attempt_history` |
|---|---|---|
| `conduct_evaluation` (conductor) | ✓ | ✓ |
| `supervisor_gate` (supervisor) | ✓ | ✓ |
| `arbiter_decide` (arbiter) | ✓ | ✓ |
| `conduct_rework` (worker) | ✗ (scope boundary) | ✓ |
| `conduct_decompose` | ✗ | ✗ |

`plan_ledger` is passed to all three conductor-facing roles unconditionally
(when non-`None`); it never depends on whether the supervisor is configured —
keeps the four call-sites uniform and auditable. Blocks are XML-isolated per the
security posture at prompts.rs:4-10; the doc-comment there will be extended to
note these new conductor-authored, mechanically-bounded blocks (they do **not**
reopen the bare-interpolation hole).

### Config (default ON — owner decision)

```rust
// config.rs — serde camelCase, Config.conductorMemory: Option<…> (serde default None)
struct ConductorMemoryConfig {
    enabled: bool,                  // Default: TRUE
    ledger_char_cap: usize,         // Default: 1500
    attempt_history_char_cap: usize // Default: 800
}
```

- **Default `enabled = true`.** The conductor remembers out of the box — this is
  the product behavior the owner wants for big, long-horizon tasks.
  `ConductorMemoryConfig::default()` and `Config::default().conductor_memory`
  both resolve to enabled. `consilium init` writes the block uncommented.
- Threaded into `ConductDeps` as a plain value. The empty-elision design keeps
  first-attempt / single-subtask prompts byte-identical even when enabled, so the
  cost is paid only where there is real prior context to carry.
- **Existing-test handling:** `ConductDeps` gains a required `memory` field, so
  every construction site is updated (compiler-enforced). Empty-elision means most
  existing tests' prompts are unchanged; any test that asserts byte-exact prompt
  text on a *multi-attempt / multi-subtask* path sets `enabled: false` to preserve
  its pre-memory assertion (it is testing legacy behavior). New memory tests use
  the enabled default.
- **Post-ship monitoring:** dogfood + early real runs measure (a) added token
  cost, (b) whether rework loops shorten, (c) folded-summary fidelity (research
  open question #5). If fidelity loss shows up, tune the caps / summary schema —
  the escape hatch is `enabled: false`, not a redesign.

### Keystone safety (grounding rule) — confirmed + pinned at two layers

The grounding override (conduct.rs:466-477) runs **before** `attempts.push`
(conduct.rs:492), so attempt history reflects the **post-override** decision by
construction. The ledger `summary`/`status` are written only at terminal sites,
all post-override. Pinned by tests #3 (attempt-history layer) **and** an
assertion that a grounding-overridden accept lands in the ledger as `failed`/
re-worked, never `completed` (req. change, optional-improvement #4).

## Tasks (T2+T3 merged → one atomic commit, req. change / opt-imp #2)

- **T1 — config**: `ConductorMemoryConfig` + `Config.conductorMemory`, `Default`
  (enabled), serde round-trip test, `consilium init` starter (uncommented,
  `enabled:true`).
- **T2+T3 — prompts + conduct wiring (atomic)**: add the four `Option<&str>`
  signatures + XML blocks; the `build_subtask_entry` helper + `last_verify`;
  route all eight terminal pushes through it; the two `render_*` fns (empty→None,
  marker-in-budget caps); thread renders into the four call-sites per the matrix;
  read caps/`enabled` from `ConductDeps`. The compiler's arity check guarantees no
  call-site is missed. Update existing prompt unit tests for the new params.
- **T4 — tests + dogfood**: integration tests (below); a real multi-subtask
  `conduct` smoke with `enabled:true` proving attempt-2's evaluation prompt
  carries attempt-1's feedback and subtask-2's prompt carries subtask-1 in the
  ledger; inspect transcript `status`/`summary`; record measured added token cost.

## Test plan (RecordingSequenced seam — feasible per review; exact step arrays given)

Spy on conductor prompts via `RecordingSequenced` (conduct_test.rs test 13,
lines 1043-1070; promote to common/mod.rs since ≥4 tests reuse it). The cursor
returns `steps[N]` for call N and logs the prompt before delegating
(common/mod.rs:127-130) — so `log[N]` = call N's prompt.

1. **attempt_history_threads_prior_feedback** — conductor steps
   `[plan, rework("add docs"), accept]`; worker mutates a file each attempt;
   memory `enabled:true`. Assert `log[1]` (attempt-0 eval) has **no**
   `<attempt_history>`; `log[2]` (attempt-1 eval) contains `<attempt_history>` and
   `.contains("add docs")`.
2. **plan_ledger_threads_prior_subtasks** — 2-subtask plan, conductor steps
   `[plan, accept(sub1), accept(sub2)]`. Assert subtask-2's eval prompt contains
   `<plan_ledger>` with subtask-1's title + `completed`; subtask-1's eval prompt
   has no ledger block.
3. **grounding_override_recorded_in_history (keystone)** — attempt-1 verify fails
   (`grep -q good out.txt`) while conductor scripted to accept; attempt-2 worker
   writes good. Assert attempt-2's `<attempt_history>` shows round-0 as
   `rework`/verify `failed` (never `accept`); AND if it had been the last
   subtask's predecessor, its ledger entry would be non-`completed` — assert the
   transcript `subtasks[0].attempts[0].decision == "rework"` and `verify ==
   "failed"` (ledger-layer keystone).
4. **ledger_respects_cap** — multi-subtask with tiny `ledger_char_cap`; assert the
   rendered `<plan_ledger>` length ≤ cap and contains the `"… elided"` marker.
5. **memory_disabled_is_byte_identical** — `enabled:false`; assert evaluation /
   rework / supervisor / arbiter prompts contain no `<plan_ledger>` /
   `<attempt_history>` blocks (today's behavior).
6. prompts.rs unit tests: each new block present iff its `Option` is `Some`,
   omitted (byte-identical) when `None`.

## Verification before done

- `cargo test` green (target ≥ 169: 163 + ≥6 new), `cargo clippy --all-targets
  -- -D warnings` exit 0 (checked directly, not via `| tail`), `cargo fmt --check`.
- Real dogfood (memory `enabled:true`): 2-subtask `conduct` on a scratch cargo
  crate; confirm transcript `status`/`summary`, cumulative rework feedback, and
  log the measured added token cost vs a disabled baseline run.
- Whole-branch adversarial review (opus) before merge.

## Known assumptions / explicit deferrals

- **Subtask file-disjointness** (prompts.rs:59) makes ledger summaries
  informational and race-free. Enforcing disjointness (a static check failing on
  overlapping change-sets) is **deferred to P0 #3**, where parallelism / shared
  edits / worktrees are actually designed. Documented here so a future author
  doesn't relax disjointness without revisiting the ledger.
- `plan_ledger` (cross-subtask status) and `supervisor_note` (current-subtask
  concern) are complementary, not redundant — both may appear in the evaluation
  prompt by design.

## Risks (post-review) + mitigations

| Risk | Mitigation |
|---|---|
| Prompt bloat / cost (worst case dominated by re-injected attempt_history) | two per-block char caps, per-call; most-recent-N elision; cost measured in T4 dogfood |
| New blocks inherit bare-interpolation steering risk | ledger.summary is mechanical-only (no worker/feedback text); all blocks XML-isolated; security header extended |
| Missing a terminal push site (the v1 bug) | single `build_subtask_entry` helper = one construction point for all 8 sites |
| Keystone erosion (pre-override decision recorded) | history rendered post-override (conduct.rs:492); pinned by test #3 at attempt + ledger layers |
| Behavior/cost change for existing users (default ON, owner decision) | empty-elision keeps no-prior-context prompts byte-identical; per-block caps bound cost; dogfood measures it; `enabled:false` escape hatch |
| Signature churn leaves tests green but behavior wrong | T2+T3 atomic; compiler arity check; integration tests validate end-to-end |
