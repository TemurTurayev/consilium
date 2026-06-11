# Consilium

> Get a second opinion. And a third.

One orchestrator for the AI coding subscriptions you already pay for. Consilium drives the **official CLI agents** — Claude Code, Codex CLI, Gemini CLI — so they can deliberate on hard problems, cross-review each other's work, and split tasks between providers without burning through any single subscription's limits.

Named after the medical *consilium*: specialists from different fields gathering around one patient.

## Why

- **Subscriptions, not API keys.** Consilium never calls provider APIs directly — it runs the official CLIs authenticated with your existing Claude Max / ChatGPT / Google plans. All their features keep working; nothing gray-zone.
- **Quota economics changed.** Since June 15, 2026, Anthropic meters `claude -p` (headless) against a separate monthly credit instead of the flat subscription pool. Consilium's architecture is built around this: heavy lifting goes to the workers with the freest quota, and the planned *attached mode* (M3) keeps the conductor inside your interactive Claude Code session via MCP, where flat subscription limits still apply.
- **Accurate accounting is the headline feature.** Provider token semantics genuinely differ — we verified each against recorded real CLI output (see table below). Most tools get at least one of them wrong.

## Status

| Milestone | Scope | State |
|---|---|---|
| **M1 — Engine foundation** | CLI adapters, session manager, quota store, `doctor`/`run`/`quota` commands | ✅ Done — 37 tests, verified E2E |
| **M2 — Orchestration** | `council` (anonymized peer review), `conduct` (conductor/workers), `review`, `auto` pipeline, supervisor | 🚧 Next |
| **M3 — Server & UI** | axum + WebSocket server, MCP attached mode, React web UI, quota dashboards | Planned |
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
```

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
- **Attached conductor** — your interactive Claude Code session orchestrates workers through Consilium's MCP server, spending subscription limits instead of the programmatic credit.

## License

[MIT](LICENSE)
