# Consilium

> Get a second opinion. And a third.

One orchestrator for the AI coding subscriptions you already pay for. Consilium drives the **official CLI agents** — Claude Code, Codex CLI, Gemini CLI — so they can deliberate on hard problems, cross-review each other's work, and split tasks between providers without burning through any single subscription's limits.

Named after the medical *consilium*: specialists from different fields gathering around one patient.

## Why

- **Subscriptions, not API keys.** Consilium never calls provider APIs directly — it runs the official CLIs authenticated with your existing Claude Max / ChatGPT / Google plans. All their features keep working; nothing gray-zone.
- **Built for shifting quota rules.** A June 2026 plan to meter `claude -p` (headless) against a separate credit was put on hold on June 17, 2026 — headless Claude currently runs on flat subscription limits, same as interactive. Consilium doesn't depend on either outcome: heavy lifting routes to the worker with the freest quota, and the *attached mode* (M3) can keep the conductor inside your interactive Claude Code session via MCP — useful insurance if metered headless usage returns.
- **Accurate accounting is the headline feature.** Provider token semantics genuinely differ — we verified each against recorded real CLI output (see table below). Most tools get at least one of them wrong.

## Status

| Milestone | Scope | State |
|---|---|---|
| **M1 — Engine foundation** | CLI adapters, session manager, quota store, `doctor`/`run`/`quota` commands | ✅ Done — verified E2E |
| **M2a — Deliberation** | `council` (anonymized peer review → chairman synthesis), `review` (diff audit with CI exit codes) | ✅ Done — verified on live providers |
| **M2b — Execution** | `conduct` (conductor decomposes → workers edit real files → review gate → arbiter), `auto` pipeline, supervisor, quota-aware routing | ✅ Done — verified on live providers |
| **M2c — Resilience** | per-role model **failover ladders**, real-error classification, run-wide `ModelHealth`, `doctor --models`, `init` | ✅ Done — verified against a live model outage |
| **Harness leveling (P0)** | build/test **grounding**, **ConductorMemory** (plan ledger + attempt history), **worker blackboard** | ✅ Done — research-backed |
| **M3a — Attached conductor (MCP)** | `consilium mcp` stdio server exposing `run_worker` + `quota_status` — your live Claude Code session is the conductor; no programmatic Claude credit spent | ✅ Done — verified over stdio |
| **M3b — Live streaming server** | `consilium serve` — axum WebSocket at `/ws/session` streams a run's events live (task-local `ProgressSink`) | ✅ Done — verified E2E over a real socket |
| **M3c — Cross-family review** | `conduct` routes a subtask's diff to a reviewer/arbiter of a *different* model family than the worker that wrote it (`crossFamilyReview`) | ✅ Done — opt-in, verified |
| **M3e — Live web UI (Slice A)** | Vite + React **Session** view over `/ws/session`; typed protocol via `ts-rs` single-source-of-truth bindings, a pure unit-tested reducer, and a zero-backend demo mode | ✅ Done — live-verified in browser |
| **M-eval — Benchmark harness (Slice A)** | `consilium eval` scores orchestration **approaches** (solo / conduct / ±grounding / ±cross-family) by an *independent* build/test verifier; dry-run by default | ✅ Harness done — live numbers are an opt-in run |
| **M3 (rest) — MCP tools, memory, dashboards** | `review_diff`/`council_run` MCP tools, memory/recitation tools, quota dashboard + Council view | 🚧 Next |
| v1.1+ | Warp terminal integration (OSC 777), Tauri desktop app | Planned |

## Quick start

Prerequisites: Rust ≥ 1.85 and at least one of the agent CLIs installed and logged in (`claude`, `codex`, `gemini`).

```bash
git clone https://github.com/TemurTurayev/consilium.git
cd consilium
cargo build --release

cargo run -q -- doctor                                    # check agent CLIs
cargo run -q -- run --provider gemini "Reply with: ok"    # single-agent smoke run
cargo run -q -- quota                                     # usage in the last 5h window

# The flagship: convene a council — workers answer independently, cross-review
# each other anonymously, the chairman synthesizes the best answer.
cargo run -q -- council "Async Rust: when is spawning a task per request wrong?"

# Audit a diff with the reviewer role. Exit codes: 0 no critical findings,
# 2 critical findings, 3 reviewer output unparseable (fails closed).
git diff | cargo run -q -- review --diff-file /dev/stdin

# Execution: a conductor decomposes the task, routes subtasks to the worker
# with the freest quota, each worker edits real files, a reviewer audits the
# diff, and a supervisor watches. Runs in a git repo.
cargo run -q -- conduct "Add a CHANGELOG.md with a 0.1.0 entry"

# The full pipeline: triage → (council plan if non-trivial) → conduct → optional
# check command (runs in a shell). Exit 1 if the run fails, is halted, or the
# check fails.
cargo run -q -- auto "Fix the typo in README.md" --check "cargo test"

# Write a starter config you can edit, then probe which configured models are
# actually reachable right now.
cargo run -q -- init
cargo run -q -- doctor --models
```

## Grounded execution: conduct trusts your tests, not vibes

`conduct` runs your real build/test/lint after each worker attempt and treats the
result as authoritative: **a subtask whose tests fail cannot be accepted** — even
if the conductor's own judgment says "looks good", a failed verifier forces a
rework. "No verifier ran" is recorded as `not_run` and the conductor is told its
judgment is unverified. Declare commands in `consilium.config.json`, or rely on
auto-detection (Cargo / npm / pytest / make):

```jsonc
// consilium.config.json
"verify": { "test": "cargo test", "build": "cargo build" }  // lint is advisory
```

Why: research on agent self-correction is clear that a model judging its own work
*without* an external verifier often degrades — so the build/test signal grounds
the whole accept/rework loop. Every attempt's verify status (`passed` / `failed`
/ `not_run`) lands in the run transcript.

## Conductor memory: it remembers across the run

By default the conductor carries a working memory **within** a run (it travels as
prompt text, so the stateless-process architecture is untouched):

- **Cumulative attempt history** — when a subtask is reworked, the conductor's
  next judgment and the worker's next attempt both see *every* prior round's
  decision + feedback, not just the last one. It stops re-issuing the same
  feedback and oscillating.
- **Plan ledger** — each subtask's conductor prompt carries a folded status line
  for the prior finished subtasks (id, title, completed/failed + verify digest),
  so the conductor has cross-subtask awareness. The ledger summary is mechanical
  only — no worker text leaks into it — and every block is XML-isolated and
  char-capped so cost stays bounded.
- **Worker blackboard** — worker N's initial prompt inherits a read-only,
  mechanical roster of the prior finished subtasks (id/title/status, no verify
  digest, no feedback) plus the files already modified this run, so it can build
  on — and avoid clobbering — what came before. Workers never see the conductor's
  feedback or attempt history.

```jsonc
// consilium.config.json — on by default; tune caps or switch off
"conductorMemory": { "enabled": true, "ledgerCharCap": 1500, "attemptHistoryCharCap": 800 }
```

Empty blocks are elided, so a first attempt / single-subtask run pays nothing.
Recorded in the transcript per subtask as `status` + `summary`. (Subtasks run
sequentially over disjoint files; per-subtask git-worktree isolation is deferred
until parallel workers land.)

## Attached conductor (MCP): your live session orchestrates the army

In *attached mode* the conductor is **your interactive Claude Code session**, not a
spawned `claude -p` — so decisions run on your flat subscription, never metered
programmatic credit. `consilium mcp` is a stdio MCP server exposing the engine's
primitives as tools your session calls:

- **`run_worker`** — route a self-contained subtask to a configured worker
  (Codex/Gemini/Claude); it edits real files (auto-approved writes, scoped) and
  returns the captured diff + build/test result. Failover ladders apply.
- **`quota_status`** — tokens used per provider in the last 5h, so you route to
  the freest subscription.

Register it in a Claude Code session (`.mcp.json` or `claude mcp add`):

```jsonc
{ "mcpServers": { "consilium": { "command": "consilium", "args": ["mcp"] } } }
```

Then ask your session to delegate: it decides *what* to hand off and whether to
accept; the engine executes. Logs go to stderr so they never corrupt the stdio
protocol. (M3a — the remaining MCP tools, the WebSocket server, and the web UI
are the next M3 slices.)

## Live run streaming (`consilium serve`)

`consilium serve` starts a localhost server with a WebSocket at `/ws/session`.
Open it, send one frame describing the run, and receive every `AgentEvent` live
as it happens, then a terminal `run_complete` frame:

```bash
consilium serve --addr 127.0.0.1:7878
# then, from a WS client:
#   → {"kind":"conduct","task":"add a CHANGELOG","cwd":"/path/to/repo"}
#   ← {"type":"tool_call",...}  {"type":"message",...}  {"type":"usage",...}  ...
#   ← {"type":"run_complete","completed":[1],"halted":null,"failed":null}
```

Live delivery rides an engine-level task-local `ProgressSink`: the run executes
inside a scoped sink that fans each event into the socket, with **zero changes
to the orchestration signatures** — `None` (CLI/tests) is a no-op, so behavior is
identical when no sink is installed. This is the seam the web UI (M3e, below) and the
memory/recitation tools (M3d) build on. First endpoint is `conduct`; council/auto
and the quota/supervisor channels follow.

## Web UI (`ui/`)

A live browser view of a `conduct` run — the council deliberating in real time.

```bash
cargo run -- serve                     # backend on 127.0.0.1:7878
cd ui && npm install && npm run dev     # UI on http://localhost:5173
```

Type a task, hit **Conduct**, and watch each event stream into a timeline keyed
by provider (Claude / Codex / Gemini accent rails). No backend handy? Hit **Demo
run** to replay a canned council session through the exact same renderer — zero
quota, zero setup.

The protocol is a single source of truth: `ts-rs` generates the TypeScript
bindings in `ui/src/protocol/` from the Rust types (`core/src/event.rs`,
`core/src/protocol.rs`), so `cargo test` regenerates them and the UI's `tsc`
build fails if they drift. The view's logic is a pure reducer (unit-tested with
Vitest); only `useSession` touches the socket. See `ui/README.md`.

## Benchmarking approaches (`consilium eval`)

Does the council + build/test grounding actually beat a solo agent? `consilium
eval` measures it honestly: it runs each task through one or more **approaches**
and scores the result with an **independent** `run_verify` (build/test) on the
produced tree — an approach's own "I'm done" is never trusted, and a trial where
no verifier ran counts as not-passed (a conservative lower bound).

```bash
consilium eval                       # DRY RUN: prints the task × approach × trial
                                     # matrix and cost estimate, calls no models
consilium eval --approaches solo,conduct,conduct-no-grounding \
               --trials 5 --spend-quota
```

It is **dry-run by default**; `--spend-quota` is required to actually call
models. Each trial runs in an isolated temp copy of the task's starter repo with
its own in-memory quota ledger (it never touches your real `~/.consilium`), so a
benchmark can't pollute your usage. Tasks live in `eval/tasks/<name>/` (a
`task.json` + a `repo/` whose committed test fails until the change is made).
Results print as a markdown table (`k/N` + stability + median tokens) and save to
JSON. The cleanest single-variable claim is `conduct` vs `conduct-no-grounding` —
same external scorer, only the grounding gate differs. See
`docs/plans/2026-06-18-m-eval-slice-a.md`.

## Cross-family review: the army checks itself

Models exhibit self-preference bias — they rate their own (and same-family)
output too highly. So when `crossFamilyReview` is on, `conduct` routes a
subtask's diff to a reviewer (and arbiter) of a **different model family** than
the worker that produced it: a Codex worker's diff is audited by Gemini or
Claude, etc. The research calls this the near-zero-cost way a multi-provider
"army" actually pays off.

```jsonc
// consilium.config.json — opt-in (default off)
"crossFamilyReview": true
```

Mechanism: the reviewer ladder is reordered so a different-family rung fronts; if
the reviewer role has no other family, a different-family worker's model is
borrowed; if the deployment is genuinely single-family, it degrades to the
same-family reviewer and marks the attempt `cross_family: "degraded_same_family"`
(fail-open — review is never blocked over disjointness). Off by default because
enabling it changes which model reviews on the stock config.

## Resilience: model failover ladders

Each role takes an ordered **ladder** of models, not one model. If the primary
returns a model-unavailable error, Consilium demotes to the next rung — loudly —
and marks the dead model so the rest of the run never retries it. A run never
falls over because one model got pulled.

```jsonc
// consilium.config.json — `consilium init` writes a starter you can edit
"conductor": {
  "provider": "claude", "model": "claude-opus-4-8",
  "fallbacks": [{ "provider": "claude", "model": "claude-sonnet-4-6" }]
}
```

This is not hypothetical. When `claude-fable-5` was withdrawn, a `conduct` run
with it as the conductor's primary recovered automatically:

```
↳ conductor fell back: claude/claude-fable-5 → claude/claude-opus-4-8 (model unavailable)
↳ conductor fell back: claude/claude-fable-5 → claude/claude-opus-4-8 (known-dead)
completed subtasks: [1]
```

Failure classification is per-provider and built from **real captured CLI error
strings** — a rate-limited model is demoted but kept alive (it may recover);
only a genuine model-unavailable error marks it dead for the run. Every demotion
lands in the run transcript.

Every deliberation writes a full JSON transcript to `~/.consilium/runs/` — including
the anonymization map and per-reviewer scores, so you can audit who said what
(and whether an agent favored its own anonymized answer).

```
$ cargo run -q -- doctor
✓ claude   2.1.111 (Claude Code)
✓ codex    codex-cli 0.139.0
✓ gemini   0.36.0
```

## How it works (M1)

```
 claude / codex / gemini CLIs          (your subscriptions)
        │ headless stdout
        ▼
 adapters/        pure parsers: raw CLI output → normalized AgentEvent
        │         (fixture-tested against RECORDED REAL outputs)
        ▼
 sessions.rs      tokio process spawn, event streaming, stderr-drain
        │
        ▼
 AgentEvent       SessionStarted · Message · Thinking · ToolCall ·
        │         FileChanged · Usage · Completed · Failed
        ▼
 quota.rs         SQLite usage log, sliding-window aggregation
```

### Token semantics actually differ per provider

Verified against recorded real CLI outputs (`core/tests/fixtures/*/recorded/`):

| Provider | Input side | Output side |
|---|---|---|
| Claude | `input + cache_creation + cache_read` — cache tokens are **disjoint** additions | `output` |
| Codex | `input` only — `cached_input_tokens` is a **subset**, summing would double-count | `output + reasoning_output_tokens` |
| Gemini | Σ over **all** internal models (`prompt + cached`) — one request may use several | Σ `candidates + thoughts` |

## Development

- TDD throughout; `cargo test` (unit + integration), `cargo clippy --all-targets -- -D warnings`, `cargo fmt`.
- Adapter parsers are tested against **recorded real CLI outputs** committed as fixtures — format regressions surface without spending quota. Re-record with `script/record_fixtures.sh` (spends a few real requests).
- Design spec: [`docs/specs/`](docs/specs/) · implementation plans: [`docs/plans/`](docs/plans/)

## Roadmap highlights

- **`council`** — the [llm-council](https://github.com/karpathy/llm-council) pattern ported to coding agents on subscriptions: independent answers → anonymized cross-review → chairman synthesis.
- **`auto`** (default mode) — council for planning, conductor for execution, cross-review per subtask, supervisor watching everything ("reads a lot, writes rarely" — input tokens are cheap).
- **Attached conductor** — your interactive Claude Code session orchestrates workers through Consilium's MCP server: richer context, and resilient to any future return of metered headless usage.

## License

[MIT](LICENSE)
