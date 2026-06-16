# Harness Leveling Research — toward Claude-Code-level long-horizon agency

*Date: 2026-06-17. Method: 5 parallel research lenses (coding harnesses, self-correction,
memory/context, multi-agent orchestration, multi-model deliberation) → synthesis. 54 findings,
each sourced and re-verified against primary sources (papers + open-source repos). Hype filtered.*

This is the design basis for **M3 (attached conductor)** and **M4 (deep harness)**. It answers:
how do we make Consilium's conductor handle big, long-horizon tasks at Claude-Code level *or
better* — self-correcting as many times as needed without runaway, using the multi-provider army,
with the owner's governance rule (conductor decides unless unanimous veto)?

## The one counterintuitive headline

**Self-correction is currently ungrounded — fix that before anything else.** Consilium's
conductor `EVALUATE` and the reviewer are *model-judging-text* with no build/test signal. The
most evidence-backed result in this whole space (Kamoi TACL 2024; Huang ICLR 2024, both
re-verified) is that **intrinsic self-correction without an external verifier does not reliably
improve and often degrades.** Corollary that reorders our instinct: **raising `MAX_REWORKS` or
adding reflection rounds *before* wiring a verifier is negative-ROI.** Ground first, then loop.

## Reordered priorities

### P0 — the foundation (all M4 "deep harness"; cheap, highest ROI)

1. **Run real build/test/lint in the worktree; make it the PRIMARY driver of accept/rework.**
   Execute detected build+test+lint in `cwd` after `capture_changes`; feed structured results
   (exit code, failing-test names, error text) into `conduct_evaluation`/`conduct_rework`. The
   conductor's opinion becomes a tiebreaker *on top of* the test signal, never a substitute.
   "No verifier ran" = a hard signal to NOT trust an Accept.
   *Targets:* conduct.rs (2c→2e), prompts.rs, new build/test runner beside changes.rs.
   *Evidence:* Kamoi/Huang (verified); CRITIC, Self-Debug, MetaGPT executable feedback (+4.2% HumanEval).

2. **Replace the dead transcript with a live `ConductorMemory` (plan ledger + folded summaries).**
   `keep_first` = task spec + decomposition (never forgotten) + a running compacted summary of
   accepted results/decisions/gotchas, persisted to `<run>/memory.md`; hydrate the conductor from
   it each stage instead of rebuilding from the JSON transcript. Each worker returns a *folded*
   structured block (changes, interfaces, learnings, risks) — that, not raw text, accumulates.
   *Targets:* transcript.rs (write-once → live memory), conduct.rs (hydrate per stage).
   *Evidence:* OpenHands rolling condenser (~2x cost cut, no quality loss); context-folding
   (arXiv 2510.11967); Mem0; Anthropic plan-in-memory.

3. **Shared blackboard so worker N inherits what workers 1..N-1 learned.** Append-only typed
   artifact store keyed by subtask id; each worker PUBLISHES a serde record (changes, decisions,
   interfaces, gotchas, follow-ups); inject relevant records into later worker prompts. Pair with
   git-worktree isolation per independent subtask.
   *Targets:* conduct.rs (worker prompt assembly), new ledger module, routing.rs (worktree-per-subtask).
   *Evidence:* MetaGPT "documents over dialogue" (85.9% HumanEval); Anthropic filesystem artifacts;
   Reflexion persisted lessons. Worktree isolation = the one battle-tested CLI-orchestrator idea.

### P1 — the adaptive loop & army payoff

4. **Two-loop conduct (Magentic-One): rewritable Task Ledger + stall-triggered replan.** Outer
   loop holds a Task Ledger the conductor can REWRITE; inner loop runs each subtask; after each,
   fill a Progress Ledger (is_complete, progress_being_made, is_in_loop, next_instruction,
   next_speaker). Keep MAX_REWORKS=2 as the inner stall cap; on global stall, *regenerate the
   plan* instead of failing. Extend `EvalDecision` with `InsertSubtask` and `Replan`. (M4)
   *Evidence:* Magentic-One (arXiv 2411.04468, verified — stall threshold ≤2, same as ours;
   ablating ledgers drops GAIA 31%); LangGraph `Command(goto, update)`.
5. **Convergence + fingerprint stagnation detection** to safely relax the hard rework cap: keep
   reworking while a metric (tests passing/failing, build_ok) strictly improves; stop on goal met;
   trip a circuit breaker on 3 identical fingerprints / ABAB oscillation / zero info-gain. (M4)
   *Evidence:* OpenHands stuck-detection; fingerprint loop detection.
6. **Layered budget governor** (run-scope rework ceiling + token/cost + wall-clock; whichever
   trips first), with a "budget pressure" signal so the conductor degrades gracefully to
   "ship best-so-far + report unfinished" rather than crashing. (M4)
   *Evidence:* Agent SDK defaults are UNLIMITED — runaway control is opt-in.
7. **Cross-FAMILY review rule** — the reviewer/arbiter ladder must be a *different model family*
   than the producing worker (Anthropic output reviewed by Gemini/Codex, etc.). (M3)
   *Evidence:* Self-Correction Illusion (+23–93 pts when error attributed to another); self-preference
   bias (ArenaHard −38%→+90%). **This is the concrete, near-zero-cost way the army pays off.**
8. **Capability-parity gate on council membership** — only convene a cross-model council among
   near-peers; never let a weaker tier (Sonnet/Haiku) vote or count in a veto; sometimes
   single-best-model self-consistency wins. (M4)
   *Evidence:* Self-MoA (mixing a weaker model LOWERS quality); "Talk Isn't Always Cheap"
   (superior agents' accuracy drops when paired with inferior ones).
9. **Recite the plan at prompt edges; externalize bulk.** Pin original task + current checklist at
   TOP, immediate instruction + recent state at BOTTOM, push bulk diffs to the middle or to
   `<run>/artifacts/` with a path+digest. Upgrade `build_progress()` into a recited checklist. (M3)
   *Evidence:* lost-in-the-middle (Liu TACL 2024); context rot (Chroma — 200k model degrades by ~50k);
   Manus recitation + filesystem-as-memory.
10. **M3 attached conductor: MCP memory tools** (page_in, search_recall over the run event log,
    write_note/archival over ledger+artifacts) + routing-command tools (NextSubtask, InsertSubtask,
    Replan, EscalateToCouncil, Done). Even a 1M attached session hits the context-rot zone. (M3)
    *Evidence:* MemGPT/Letta OS-style memory tiers; Live-SWE-agent 75.4% SWE-bench; just-in-time retrieval.

### P2 / later
- Anonymize the conductor's EVALUATE step + Borda over the council rating matrix (M4).
- Chairman SELECTS-then-patches with a typed {consensus, contradictions, blind_spots, final_answer}
  schema instead of free-prose blending (M4). *Evidence:* selection beats synthesis (0/42 tasks preferred blend).
- Worker-local self-debug tier (run tests, debug ≤3) below the conductor rework tier (M4).
- Cap ungrounded review/critique rounds at 1–2; reserve long iteration for grounded code-rework (M4).
  *Evidence:* "More Rounds, More Noise"; Self-Refine (94% of failures originate in the feedback step).
- Keep role system-prompt prefixes byte-stable across stages for CLI prefix-cache hits (M3).
- Architect/Editor split: reasoner plans, cheap precise model materializes diffs (later).
  *Evidence:* Aider Architect/Editor 85.0% vs 79.7% solo.
- Best@k parallel attempts for hard subtasks selected by tests (later) — reserve for genuinely
  independent subtasks (coding is ~15x tokens, less parallelizable than research).

## Governance verdict (the owner's rule)

**Adopt "conductor has the last word unless all council participants unanimously veto" — it is
well-founded, with two refinements.** Leader-with-veto helps *only when the leader is the strongest
model* (a weak central aggregator actively underperforms) — so make "conductor is the strongest
model for this decision" an explicit invariant. The **unanimous** threshold is provably safer than
majority: one confident-wrong agent can swing a majority and drop accuracy 10–40%, whereas a
unanimous veto requires ALL models to be wrong together (far rarer). Refinements: (a) **anonymize**
the conductor's judging and the veto vote (self-preference bias is real) and **capability-parity-gate**
the council; (b) on a veto, do NOT auto-flip to majority — re-run as a fresh parity council where the
conductor must DEFEND, chairman SELECTS (not blends), and a Borda tally confirms the opposition is
genuinely unanimous. Anchor the whole decision to executed build/test signal where one exists
(debate without a corrective signal is a martingale).

## Do NOT build (hype filtered, with reasons)

- **Mass parallel coder subagents as the default** — coding is less parallelizable than research,
  ~15x tokens; the army's edge is the build/test oracle, memory, and cross-model critique, not fan-out.
- **Auto-including every model in a council** — Self-MoA: quality is dominated by the worst participant.
- **More review/debate rounds for accuracy** — plateaus then degrades (error propagation).
- **Raising MAX_REWORKS / adding reflection before wiring a verifier** — ungrounded loops degrade.
- **Chairman free-prose blending** — Frankenstein merges no model endorses; select-then-patch.
- **CLI-orchestrator marketplace cosmetics** ("39 specialists", agent identity, kanban) — demo-grade;
  take only git-worktree isolation.
- **Full MCTS/LATS tree search** — steal the replan-on-feedback pattern, skip the expensive width.
- **Treating a 1M-context attached conductor as removing memory management** — context rot hits first.
- **karpathy llm-council "stronger than best single model"** — a vibe-coded weekend project with no
  perf data; a hypothesis to validate in our own transcripts, not evidence.
- **Heavyweight A-MEM / full MemGPT paging as v1** — Mem0-style extract-then-update over a flat ledger
  delivers most of the value for far less complexity.

## Open questions (need our own data to settle)

1. Reliable per-repo build/test/lint detection (Cargo vs npm vs pytest vs make); fail-closed on
   "no tests ran" vs a genuinely test-free repo.
2. Epsilon / stagnation thresholds — needs empirical calibration on Consilium's own transcripts
   before relaxing MAX_REWORKS.
3. Cross-family disjointness vs failover exhaustion (reviewer family runs out — same-family-anonymized,
   or fail the review?). Interaction with ModelHealth.
4. Self-MoA (single-best self-consistency) vs cross-family critique — reconcile as self-consistency
   for GENERATION, cross-family for CRITIQUE; measure in our setting.
5. Folded-summary fidelity — schema that retains enough to replan correctly (Anthropic's own warning
   about dropping subtle-but-later-critical context).
6. M3 attached-conductor token economics — does persistent-memory saving offset the attached-loop cost,
   and at what task size does attached go net-positive?
7. Governance invariant enforcement — how to MEASURE "conductor is strongest for this task" rather than
   assume Opus always is; should the conductor role rotate per task type?
8. Worktree merge-back conflict resolution for best@k parallel diffs.
