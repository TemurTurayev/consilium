# M-eval (Slice A) — benchmark harness for orchestration approaches

> Status: IN PROGRESS. M-eval measures whether Consilium's orchestration
> (council + grounding) actually beats a solo agent, by running coding tasks
> through different **approaches** and scoring each with an **independent**
> build/test verifier. Slice A ships the harness + `consilium eval` + zero-quota
> tests; it does NOT commit any live "council beats solo" numbers (that needs a
> budgeted, operator-run `--spend-quota` pass).

## Why

The harness-leveling research and the Ponytail-benchmark review both flag the
same gap: we *claim* council/grounding help, but have no numbers, and the honest
way to get them is a verifier-scored ablation, not vibes. The thesis ("debate
without a corrective signal is a martingale") predicts the **build/test grounding**
is what moves the needle — so the cleanest, most-defensible comparison is
`Conduct` vs `ConductNoGrounding`, scored by the *same external oracle*.

## The honesty keystone

Every approach is scored by the harness re-running `run_verify` **after** the
approach finishes, on the resulting tree. `success = verify.ran && verify.passed`.
The approach's own "I completed" is recorded but is NOT the score — a conduct run
that reports success but leaves a broken build scores **false**. A trial where no
verifier ran counts as **not-passed** (conservative lower bound; surfaced as an
"unscored" bucket, never folded into the pass rate).

## Approaches (each isolates one variable)

- **`solo`** — one worker model on the full task prompt via `run_with_failover`
  (`advisory:false, write:true`), no decompose/gates. The real-world baseline.
- **`conduct`** — the full pipeline (`run_conduct`): decompose → workers →
  supervisor → review → arbiter → build/test grounding.
- **`conduct-no-grounding`** — `conduct` with `verify:None`. `conduct` ↔
  `conduct-no-grounding` is the cleanest single-variable claim (same external scorer).
- **`conduct-cross-family`** — `conduct` with `cross_family_review:true`.

Slice A wires + tests **solo** and **conduct**; the enum carries all four,
selectable via `--approaches`. (Auto/Council-as-answer are out: they don't
produce a verifiable code change the way conduct does.)

## Safety (this is what lets it ship)

`consilium eval` is **dry-run by default**: it prints the task×approach×trial
matrix and a rough per-approach model-call estimate, and **calls no models**.
`--spend-quota` is required to actually run. Each trial uses a fresh
`QuotaStore::open_in_memory()` — the harness **never** touches the real
`~/.consilium/usage.db`, and token deltas are isolated per trial.

## Trial flow

Per (task, approach, trial): copy the task's `repo/` to a temp dir → `git init` +
baseline commit (so `capture_changes`/`git diff HEAD` works) → snapshot wall +
in-memory token totals → run the approach → **independently** `run_verify` →
record `TrialResult { success, verify_ran, pipeline_ok, tokens, wall_ms, error }`.
N trials per cell (live models are nondeterministic; report median + stability).

## Task fixture format

`eval/tasks/<name>/` with `task.json` (`name`, `prompt`, optional `context`,
optional `verify: {build,test,lint}` → `VerifyConfig`; omitted ⇒ autodetect;
optional `protected_paths`) and a `repo/` starter dir copied fresh per trial. The
starter **must fail** its verifier before the change (a real pass/fail oracle).
Slice A ships one example, `add-greeting` (a Rust lib whose committed integration
test in `tests/greeting.rs` fails until `greeting()` exists).

**Verifier integrity (`protected_paths`).** The scorer is gameable if an approach
can delete or rewrite the test it is judged on (e.g. deleting a Rust test makes
`cargo test` pass with "0 tests"). So before scoring, the harness restores every
`protected_paths` entry from the baseline commit (`git checkout HEAD -- <path>`),
undoing any worker edits. Put the test/oracle in a protected file the prompt tells
the worker not to touch (the example protects `tests/greeting.rs`). Fixtures must
contain only plain files (copy skips symlinks + build dirs like `target/`).

## Aggregation + report

Per (task, approach): `k/N`, stability (all trials agree), median tokens, median
wall, unscored count. Per approach overall: `k/N`, median tokens/wall. Outputs:
a JSON results file (`--out`, default under `eval/results/`, gitignored) + a
markdown table to stdout. Wording: conservative lower bound, method-independent
(same oracle) vs method-dependent claims labeled, prefer `k/N (stable)` over a
bare %, whole-suite (no cherry-picking), footer states N + caps.

## File manifest

CREATE: `core/src/orchestrator/eval.rs` (harness + pure aggregation + in-module
unit tests); `eval/tasks/add-greeting/{task.json,repo/Cargo.toml,repo/src/lib.rs}`;
`core/tests/eval_test.rs` (zero-quota scripted-adapter integration tests);
this plan doc.
MODIFY: `core/src/orchestrator/mod.rs` (`pub mod eval;`); `core/src/main.rs`
(`Command::Eval`); `core/Cargo.toml` (promote `tempfile` to a dep); `.gitignore`
(`eval/results/`); `README.md`.

## Tests (zero quota, the only ones in CI)

Scripted adapters mutate a temp git repo; in-memory quota. Cover: the **keystone**
(pipeline-completes-but-verify-fails ⇒ success=false), passing ⇒ success=true,
pure aggregation (`k/N` + median + stability), token-delta capture, report JSON
shape, and `dry_run_plan` calls nothing. The live matrix is operator-invoked,
never a CI gate.

## Verification

`cargo test` green; `cargo clippy --all-targets -- -D warnings`; `cargo fmt --check`.
`consilium eval` (no flag) dry-runs the example suite, spends nothing. Whole-branch
adversarial review before merge.

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| Live run quota cost | `--spend-quota` off by default; dry-run prints cost; N=1 default; one example task |
| Harness spends quota in tests | scripted adapters + in-memory quota; `dry_run_plan` is pure; never opens the real db |
| Live-model nondeterminism | N trials, median + `k/N (stable)` |
| Task-selection bias | run whole suite, report every task, prefer the method-independent claim |
| Trusting an approach's self-report | always re-run `run_verify` externally; keystone test proves it |
| Verifier-cheat (delete/rewrite the test) | `protected_paths` restored from baseline before scoring; tamper test proves it |
| temp/git fragility | explicit `GIT_AUTHOR/COMMITTER` env; `git init`+commit per trial |
| Timed-out write worker orphaned (pre-existing M1 policy) | a timeout yields an unreliable trial (a still-writing orphan can race the score); use a generous timeout. The engine-wide kill-on-timeout fix is backlogged, not in this slice |

## Deferred

More approaches (auto, best@k), bigger/public suite, significance tests
(bootstrap/McNemar), per-task difficulty weighting, parallel trials, charts, CI
integration of the live run, and committing any live numbers (a separate,
reviewed operator change).
