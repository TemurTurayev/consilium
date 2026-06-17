# P0 #3 — Worker blackboard (worktree isolation deferred)

> Status: PLAN (awaiting approval). Last slice of the harness-leveling P0 trio
> (`docs/research/2026-06-17-harness-leveling-research.md`). P0 #1 (build/test
> grounding) and P0 #2 (ConductorMemory) are merged. Grounded by an
> understand-phase fan-out + scope-decision agent.

## Decision: ship the blackboard, DEFER worktree isolation

The research bundled "worker blackboard + worktree isolation." The understand
phase says **split them and defer the worktree**:

- Subtasks run strictly **sequentially** (conduct.rs) over **disjoint files**
  (the decompose prompt mandates it), and P0 #1's post-worker `verify` runs
  before the next worker starts — so worktree isolation removes **no present
  bug** (the TimedOut-orphan concurrent-write race can't occur with one worker
  at a time).
- Worse, per-worktree isolation would start each worker from `HEAD`, which
  **breaks the cross-subtask inheritance** the blackboard exists to deliver
  (worker N could no longer see 1..N-1's actual files in the shared tree).
- Its only payoff is enabling **future parallel workers** — outside the P0 trio.

So P0 #3 = **worker blackboard only**. Interim guard for the disjointness
assumption: a doc comment on `run_conduct` stating sequential-shared-cwd is safe
because subtasks are disjoint, and worktree-per-subtask is deferred until real
parallelism lands.

## What the blackboard is

Today the **initial** worker prompt is the raw `subtask.prompt.clone()`
(conduct.rs:289-290) — no wrapping. The decompose prompt explicitly tells the
conductor "workers cannot see this conversation, each other, or earlier
subtasks." P0 #3 **relaxes that for a mechanical, read-only roster** so worker N
can build on (and avoid clobbering) what 1..N-1 produced.

Injected into the **initial** worker prompt only, as a `<prior_work>` block with
two mechanical signals:

1. **Prior-subtask roster** — one line per finished subtask:
   `- subtask {id} "{title}": {status}`. Reuses the P0 #2 `subtask_entries`
   accumulator. Status only — **no** verify digest (conductor signal), **no**
   feedback, **no** attempt history, **no** diffs, **no** worker prose.
2. **Files modified this run** — the current changed-file path list (mechanical,
   from `git status --porcelain`), baselined at run start so it shows only files
   *this run* touched. Tells worker N which files already exist / not to clobber.

Both are mechanical (injection-safe), XML-isolated via the existing
`memory_block`, char-capped via `fold_lines`, and **elided when empty** — so a
clean-start subtask 1 (and `memory.enabled=false`) gets a prompt **byte-identical
to today's** raw `subtask.prompt`.

### Why not per-subtask file attribution / `depends_note`?

- **Current-changed-file list, not per-subtask attribution:** attribution would
  need a before/after snapshot stored on each of the 8 terminal entry sites (8
  call-site changes + git overhead). The current working-tree list (one git call
  at prompt-build, minus a run-start baseline) is simpler, robust, and is exactly
  the "what exists now" state a generative worker needs. Disjoint + sequential
  makes "this run's changes so far" an accurate inheritance signal.
- **Not gated behind `depends_note`:** that field is inert dead metadata
  (declared, deserialized, never read) and is conductor-authored free text —
  gating on it would reintroduce the prose-leak the mechanical-content rule
  forbids. Always-on, gated only by `memory.enabled` + empty-elision.

## Design

### prompts.rs
```rust
pub fn conduct_initial(subtask_prompt: &str, prior_work: Option<&str>) -> String {
    match prior_work {
        None => subtask_prompt.to_string(),                 // byte-identical to today
        Some(pw) => format!(
            "{subtask_prompt}\n\n\
             Earlier subtasks in THIS run are already done (read-only context — your \
             work must not overlap their files):\n<prior_work>\n{pw}\n</prior_work>"
        ),
    }
}
```
- Update the SECURITY header + reframe the rework isolation invariant
  (prompts.rs comment/test): workers never see cross-subtask **feedback /
  attempt_history** (prose-bearing); they MAY see the mechanical `prior_work`
  roster. Keep the rework `<plan_ledger>` exclusion.

### conduct.rs
```rust
const BLACKBOARD_ELIDED: &str = "(… earlier subtasks elided)";

// roster (status only) + "files modified this run" — mechanical, None when empty
fn render_blackboard(prior_entries: &[Value], changed_files: &[String], cap: usize) -> Option<String>
fn mem_blackboard(on: bool, prior_entries: &[Value], changed_files: &[String], cap: usize) -> Option<String>
```
- Before the subtask loop: `let run_start_files = changes::capture_changed_files(&cwd).unwrap_or_default();`
- At the per-subtask prompt seam (replacing the bare clone at 289-290):
```rust
let original_prompt = subtask.prompt.clone();
let changed = changes::capture_changed_files(&cwd).unwrap_or_default();
let changed_this_run: Vec<String> = changed.into_iter().filter(|f| !run_start_files.contains(f)).collect();
let blackboard = mem_blackboard(mem_on, &subtask_entries, &changed_this_run, ledger_cap);
let mut current_prompt = prompts::conduct_initial(&original_prompt, blackboard.as_deref());
```
- Rework sites (361/683/736) unchanged — rework keeps its focused fix context
  (changes + feedback + attempt_history); the roster is initial-prompt only.
- Reuse `ledger_cap` (no new config field). `mem_blackboard` returns `None` when
  `!on`, so memory-off is byte-identical.

### changes.rs
```rust
/// Read-only list of paths with uncommitted changes (modified + untracked),
/// sorted+deduped. `git status --porcelain --untracked-files=all`. Best-effort:
/// callers degrade to empty on error (cosmetic context, never load-bearing).
pub fn capture_changed_files(cwd: &Path) -> anyhow::Result<Vec<String>>
```

## Tasks (one atomic commit)
- **T1** changes.rs `capture_changed_files` + unit test (a modified + an untracked
  file appear; clean tree → empty).
- **T2** prompts.rs `conduct_initial` + reframed invariant + SECURITY note + unit
  tests (None → byte-identical; Some → framed `<prior_work>`; only mechanical
  content).
- **T3** conduct.rs `render_blackboard`/`mem_blackboard`/marker + run-start
  baseline + prompt-seam wiring + run_conduct doc comment (disjointness +
  worktree deferral).
- **T4** integration tests + real dogfood.

## Test plan (RecordingSequenced seam, like P0 #2)
1. **blackboard_threads_prior_subtasks_to_worker** — 2-subtask plan; record the
   WORKER role. Assert subtask-1's initial worker prompt has no `<prior_work>`;
   subtask-2's contains `<prior_work>` with subtask 1's id/title/`completed` and
   the file subtask 1 created.
2. **blackboard_is_mechanical_only** — assert the worker prompt's `<prior_work>`
   never contains the conductor's feedback text or `(verify:` digest.
3. **blackboard_disabled_is_byte_identical** — `memory.enabled=false`: worker
   prompts contain no `<prior_work>`; subtask-1 prompt equals the raw
   `subtask.prompt`.
4. unit: `render_blackboard` empty→None, cap+marker, roster line format;
   `capture_changed_files` modified+untracked.

## Verification
- `cargo test` (target ≥ 192: 186 + ≥6), `cargo clippy --all-targets -- -D
  warnings` exit 0 (checked directly), `cargo fmt --check`.
- Real dogfood: a ≥2-subtask `conduct` on a scratch cargo crate; confirm a later
  worker's recorded prompt / the transcript shows it was handed the prior-subtask
  roster + changed files.
- Whole-branch adversarial review before merge.

## Risks + mitigations
| Risk | Mitigation |
|---|---|
| Invariant inversion (workers were forbidden the ledger) | reframe to feedback/history-exclusion; keep rework ledger-exclusion test; mechanical roster only |
| Prose-leak via title | titles are short conductor labels; roster is status-only; no feedback/verify; XML-isolated |
| Disjointness unenforced (awareness ≠ prevention) | documented assumption; worktree enforcement deferred to parallelism slice |
| `capture_changed_files` overhead / fresh-repo edge | best-effort `unwrap_or_default`; 1 call/subtask; no commits/revs needed (porcelain works pre-commit) |
| Empty-equivalence drift | `conduct_initial(None)` returns the prompt verbatim; pinned by a byte-identity test |
