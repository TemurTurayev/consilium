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
| **M2c — Resilience** | per-role model **failover ladders**, real-error classification, run-wide `ModelHealth`, `doctor --models`, `init` | ✅ Done — 151 tests, verified against a live model outage |
| **M3 — Server & UI** | axum + WebSocket server, MCP attached mode, React web UI, quota dashboards | 🚧 Next |
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
