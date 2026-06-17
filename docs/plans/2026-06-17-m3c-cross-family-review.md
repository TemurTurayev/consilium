# M3c (Slice A) — Cross-family review routing

> Status: IMPLEMENTED. M3c was scoped (understand+decide workflow) into Slice A
> (cross-family review in conduct — the general harness win) and Slice B (extra
> MCP tools). This is Slice A. Slice B (`review_diff` / `council_run` MCP tools)
> is deferred.

## Why (research Finding 7)

Models over-rate their own and same-family output (self-preference bias;
self-correction works far better when the error is attributed to *another*
model). So routing a worker's diff to a reviewer of a **different model family**
is "the concrete, near-zero-cost way the multi-provider army pays off." It is a
**general conduct harness win** — it improves every detached `conduct` run, not
just attached/MCP mode — which is why it ships separately from (and before) the
MCP tools.

## The rule (reorder-not-reject, fail-open)

At the review gate (and arbiter gate), knowing the worker that produced the diff:
1. Front the reviewer/arbiter ladder with its own **different-family** rungs
   (stable order).
2. Append **different-family worker** primaries as extra fallbacks (so a
   single-rung same-family reviewer — the stock config — still gets a
   cross-family option: a Codex worker's diff → the Gemini worker reviews).
3. Append the role's **same-family** rungs last (fail-open: if every
   cross-family model is dead at runtime, the review still runs).

`degraded` = no different family existed at all → run same-family, and mark the
attempt `cross_family: "degraded_same_family"` (never fail the review over
disjointness). Otherwise mark `"applied"`. Implemented as a pure helper
`cross_family_ladder(role_ladder, workers, worker_provider) -> (Vec<Rung>, bool)`
(immutable — returns a new Vec; `Rung` gained `#[derive(Clone)]`, a cheap `Arc`
clone). Same helper at both the review gate (conduct.rs review branch) and the
arbiter gate.

## Config — opt-in, default OFF

`Config.cross_family_review: bool` (`#[serde(default)]`, camelCase
`crossFamilyReview`), threaded into `ConductDeps.cross_family_review`. **Default
off** because the stock config has a guaranteed same-family collision (workers =
Codex+Gemini, reviewer = Codex), so always-on *would* change which model reviews
a Codex-worker diff — silently altering behavior and risking the existing
conduct/review tests. Off ⇒ the gates pass the ladder through unchanged and emit
no `cross_family` marker, so all 213 prior tests stay byte-identical. Flip the
default ON in a later slice after validating the worker-pool-reviewer fallback in
practice.

## Scope / deferred

- **In:** the rule at the review + arbiter gates, the config flag, the transcript
  marker. **No** `council.rs` change (council already anonymizes answers to guard
  self-preference; cross-family there is lower-value + higher-churn — deferred).
- **Deferred (Slice B / later):** `review_diff` + `council_run` MCP tools
  (`worker_status` dropped as low-value — no live registry); flipping the default
  to ON; a dedicated `reviewers: Vec<RoleConfig>` pool (the reorder + worker-pool
  fallback avoids it for now); strict cross-family *enforcement* at the MCP
  boundary (advisory there — the server doesn't know which worker produced a
  passed diff; enforcement lives here in detached mode).

## Tests

- Unit (`conduct.rs`): `cross_family_ladder` fronts a different family;
  degrades + flags when single-family; no-op when the reviewer is already a
  different family.
- Integration (`conduct_test.rs`):
  - `cross_family_review_routes_to_a_different_family` — the **same-family
    reviewer is wired to FAIL**, so a clean run + `cross_family: "applied"` proves
    the different-family worker fronted the review.
  - `cross_family_degrades_same_family_when_no_other_family` — single-family →
    `cross_family: "degraded_same_family"`, review still clean.
- Config: flag defaults off + parses.

## Verification

- `cargo test` green (219, +6); `cargo clippy --all-targets -- -D warnings` exit
  0; `cargo fmt --check`. 213 prior tests byte-unchanged (default-off).
- Whole-branch adversarial review before merge.

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| Always-on changes stock-config reviewer | default OFF; marker only when ON ⇒ prior tests byte-identical |
| Borrowed worker ladder used as reviewer (write intent?) | `run_review_ladder` builds its own `advisory:true, write:false` RunRequest — the borrowed adapter runs read-only |
| All cross-family models dead at runtime | same-family rungs appended last ⇒ review still runs (fail-open) |
| Transcript shape churn | `cross_family` key emitted only when the flag is ON |
