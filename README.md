# Consilium

> Get a second opinion. And a third.

One orchestrator for the AI coding subscriptions you already pay for. Consilium drives the **official CLI agents** — Claude Code, Codex CLI, the Antigravity CLI (`agy`, which drives Gemini), and (experimental/beta) xAI's Grok Build CLI (`grok`) — so they can deliberate on hard problems, cross-review each other's work, and split tasks between providers without burning through any single subscription's limits.

Named after the medical *consilium*: specialists from different fields gathering around one patient.

## Why

- **It automates the workflow you already do by hand.** Plenty of developers already plan a feature with Claude (best code quality, deep analysis) and then hand the spec to Codex/Gemini to implement (cheaper, more throughput) — switching tools and copy-pasting by hand. Consilium does it in one command: the **smartest model conducts** (decomposes the task, reviews the diffs), **cheaper models build**, and a build/test signal keeps everyone honest. You set the best model per role, and swap it as the landscape shifts (today's strongest coder won't be next month's).
- **Subscriptions, not API keys.** Consilium never calls provider APIs directly — it runs the official CLIs authenticated with your existing Claude Max / ChatGPT / Google plans. All their features keep working; nothing gray-zone.
- **Built for shifting quota rules.** A June 2026 plan to meter `claude -p` (headless) against a separate credit was put on hold on June 17, 2026 — headless Claude currently runs on flat subscription limits, same as interactive. Consilium doesn't depend on either outcome: heavy lifting routes to the worker with the freest quota, and the *attached mode* (M3) can keep the conductor inside your interactive Claude Code session via MCP — useful insurance if metered headless usage returns.
- **Accurate accounting is the headline feature.** Provider token semantics genuinely differ — we verified Claude's and Codex's against recorded real CLI output (Gemini via `agy` is plain-text, so its tokens are estimated — see table below). Most tools get at least one of them wrong.

## Compared to a hosted orchestrator (e.g. Sakana Fugu)

Sakana's **Fugu** (June 2026) validates the core bet: it ships a multi-agent system *as a model* — an LLM that orchestrates a pool of others with Thinker/Worker/Verifier roles and a learned "Conductor" — and it trades blows with the frontier on coding and reasoning benchmarks. Multi-agent orchestration works.

Consilium takes the opposite *delivery* model, and that's the point:

| | Sakana Fugu | Consilium |
|---|---|---|
| **Access** | Closed, hosted, usage-metered (subscription or per-token) | Open-source, self-hosted |
| **Cost** | Pay the vendor per token | The flat-rate subscriptions you already pay for |
| **Model pool** | A fixed internal pool — not an orchestrator of the external frontier CLIs (Claude Code / Codex / Gemini) you already pay for | Orchestrates the actual frontier CLIs you're already paying for |
| **Lock-in** | Trades model lock-in for single-vendor lock-in | None — you own the council and swap any role |
| **Transparency** | Black box | Every prompt, diff, verdict, and token is local and inspectable |

Fugu proves the approach pays off. Consilium runs the same idea on the models a hosted pool can't reach, on subscriptions you already have, with nothing hidden — and you can read every line of how it decides.

## Status

**v0.2.0 — beta.** One-line install on macOS (Apple Silicon + Intel) and Linux (see [Install](#install)). The orchestration engine, resilience/failover, grounded execution, onboarding (`init` / `auth`), and the MCP + live-streaming + web-UI surfaces are all shipped and verified on live providers; `conduct` was benchmarked at solo's pass-rate on ~⅓ the Claude tokens.

| Milestone | Scope | State |
|---|---|---|
| **M1 — Engine foundation** | CLI adapters, session manager, quota store, `doctor`/`run`/`quota` commands | ✅ Done — verified E2E |
| **M2a — Deliberation** | `council` (anonymized peer review → chairman synthesis), `review` (diff audit with CI exit codes) | ✅ Done — verified on live providers |
| **M2b — Execution** | `conduct` (conductor decomposes → workers edit real files → review gate → arbiter), `auto` pipeline, supervisor, quota-aware routing | ✅ Done — verified on live providers |
| **M2c — Resilience** | per-role model **failover ladders**, real-error classification, run-wide `ModelHealth`, `doctor --models`, `init` | ✅ Done — tested against captured real model-unavailable errors |
| **Harness leveling (P0)** | build/test **grounding**, **ConductorMemory** (plan ledger + attempt history), **worker blackboard** | ✅ Done — research-backed |
| **M3a — Attached conductor (MCP)** | `consilium mcp` stdio server exposing six tools: `run_worker`, `quota_status`, `review_diff`, `council_run`, `search_recall`, `page_in` — your live Claude Code session is the conductor; no programmatic Claude credit spent | ✅ Done — verified over stdio |
| **M3b — Live streaming server** | `consilium serve` — axum WebSocket at `/ws/session` streams a run's events live (task-local `ProgressSink`) | ✅ Done — verified E2E over a real socket |
| **M3c — Cross-family review** | `conduct` routes a subtask's diff to a reviewer/arbiter of a *different* model family than the worker that wrote it (`crossFamilyReview`) | ✅ Done — opt-in, verified |
| **M3e — Live web UI (Slice A)** | Vite + React **Session** view over `/ws/session`; typed protocol via `ts-rs` single-source-of-truth bindings, a pure unit-tested reducer, and a zero-backend demo mode | ✅ Done — manually verified in browser; unit-tested reducer + ts-rs bindings |
| **M-eval — Benchmark harness (Slice A)** | `consilium eval` scores orchestration **approaches** (solo / conduct / ±grounding / ±cross-family) by an *independent* build/test verifier; dry-run by default | ✅ Done — first live run (N=1, 4 tasks): `conduct` = solo pass-rate on ~⅓ the Claude tokens |
| **Fan-out DAG (Phase A)** | `conduct` subtasks carry explicit `depends_on` edges, run in dependency-order **waves**, and a failed subtask **isolates** to its dependents (recorded `skipped`) instead of aborting the run | ✅ Done — sequential; per-wave parallelism + worktree isolation is Phase B |
| **Onboarding foundation** | curated provider **catalog** (per-role recommendation scores + auth metadata) + a pure **recommendation resolver** (authed+available → best-model-per-role `RolesConfig`, graceful single-provider degradation) | ✅ Done — `consilium init` wiring + auth wizard are follow-on slices |
| **Auth orchestrator** | `consilium auth` — probes each provider's liveness and prints the exact "detect + guide" next step (`claude setup-token` / `codex login` / `agy login`); concurrent probes | ✅ Done — the `init` wizard (slice 4) consumes it |
| **Onboarding wizard** | `consilium init` — interactive: preview the recommended council → auth providers (detect + guide, degrade to what's ready) → write `consilium.config.json`; `--yes` writes the recommended lineup non-interactively | ✅ Done — completes the pick-your-council onboarding |
| **M3d — MCP tools & memory** | `council_run` MCP tool, `search_recall` + `page_in` memory/recitation tools | ✅ Done — shipped in M3a server |
| **Web UI — Usage dashboard** | per-provider token-usage panel (`GET /api/quota` + React, with `(est.)` markers for estimated tokens) | ✅ Done |
| **Desktop app (Tauri 2)** | embedded server (port 0), native folder picker, run/cancel from the window; dmg/AppImage/deb built on release tags | ✅ Done — E2E-verified on macOS |
| **Table view — the council as a scene** | pixel-art creatures per provider around the "patient" (the task), live statuses from the event stream, start/stop from the scene | ✅ Done — demo run works with zero backend |
| **Operator controls** | pause / resume / interject: the run parks at the next subtask boundary; your note lands in the conductor's memory (`operator_note` on the wire) | ✅ Done — 413 core tests |
| **Grok provider (experimental)** | `grok` CLI adapter audited against the real binary (0.2.87); flags, model id, and the 402 error shape pinned from recorded output | ✅ Code-ready — needs a SuperGrok subscription for live runs |
| **Web UI — Council view** | a live view for `council` deliberations (the Table scene covers `conduct`; anonymized peer-review runs still render as raw events) | 🚧 Next |
| v0.3+ | Kimi + GLM seats (kimi-cli adapter; claude-CLI env-override path), run history + scene replay, eval v2 with hard tasks | Planned |
| v1.1+ | Warp terminal integration (OSC 777), Godot standalone client | Planned |

## Install

You need at least one agent CLI installed and authenticated (`claude`, `codex`, or `agy`). Pick the install path that suits you:

### 0. Desktop app — macOS / Linux (beta)

A Tauri desktop app with the full web UI built in: pick a project folder in a
native dialog, run and cancel conducts live, check provider auth, and watch
per-provider quota — no terminal needed after setup. Grab the `.dmg`
(macOS arm64/Intel) or `.AppImage`/`.deb` (Linux) from
[releases](https://github.com/TemurTurayev/consilium/releases/latest);
SHA256 sidecar files ship next to every artifact.

_Unsigned beta builds: on first launch macOS requires right-click → Open.
The app embeds the same `consilium` engine — agent CLIs still need to be
installed and authenticated. Native Windows is not supported yet (the engine
drives Unix CLIs); use WSL with the CLI install below._

### 1. Prebuilt binary — macOS / Linux (no Rust needed)

```sh
curl -fsSL https://raw.githubusercontent.com/TemurTurayev/consilium/main/install.sh | sh
```

_(macOS arm64 + Intel and Linux x86_64 binaries ship with every release — currently [v0.2.0](https://github.com/TemurTurayev/consilium/releases/latest).)_

The script auto-detects your platform (macOS arm64/x86\_64, Linux x86\_64), installs the binary to `~/.local/bin/consilium`, and tells you if you need to add it to `$PATH`.

### 2. With Rust (works today, no git clone needed)

```sh
cargo install --git https://github.com/TemurTurayev/consilium consilium
```

### 3. From source

```sh
git clone https://github.com/TemurTurayev/consilium.git
cd consilium
cargo build --release
# binary is at target/release/consilium — add it to your PATH or run via cargo run -q --
```

### 4. Windows — use WSL

Consilium drives the official agent CLIs, which are Linux/macOS-first. On Windows, run it inside [WSL](https://learn.microsoft.com/windows/wsl/install) (a real Ubuntu inside Windows): open your WSL shell and follow the **Prebuilt binary** or **Rust** steps above — everything works exactly as on Linux. _(Native Windows isn't supported yet: the build/test grounding shells out to `sh`, and the agent CLIs are `.cmd` shims Rust won't spawn directly.)_

---

Once installed, start here:

```bash
consilium init      # set up your council — pick models, authenticate, done
```

## Quick start

Prerequisites: at least one of the agent CLIs installed and logged in (`claude`, `codex`, `agy`). The `gemini` provider is driven through Antigravity's `agy` CLI (the standalone Gemini CLI is retired).

```bash
# Start here: the onboarding wizard. Pick your council (or accept the Default),
# authenticate the providers it needs (it detects what's missing and tells you
# the exact command to run), and it writes consilium.config.json for you.
consilium init

consilium doctor                                    # check agent CLIs
consilium run --provider gemini "Reply with: ok"    # single-agent smoke run
consilium quota                                     # usage in the last 5h window

# The flagship: convene a council — workers answer independently, cross-review
# each other anonymously, the chairman synthesizes the best answer.
consilium council "Async Rust: when is spawning a task per request wrong?"

# Audit a diff with the reviewer role. Exit codes: 0 no critical findings,
# 2 critical findings, 3 reviewer output unparseable (fails closed).
git diff | consilium review --diff-file /dev/stdin

# Execution: a conductor decomposes the task, routes subtasks to the worker
# with the freest quota, each worker edits real files, a reviewer audits the
# diff, and a supervisor watches. Runs in a git repo.
consilium conduct "Add a CHANGELOG.md with a 0.1.0 entry"

# The full pipeline: triage → (council plan if non-trivial) → conduct → optional
# check command (runs in a shell). Exit 1 if the run fails, is halted, or the
# check fails.
consilium auto "Fix the typo in README.md" --check "cargo test"

# Check provider auth on demand (probes liveness; prints the exact login step
# for anything not ready), and probe which configured models are reachable.
consilium auth
consilium doctor --models

# Stay on the latest models: probe each provider's current top model and adopt
# it. Run this after a provider ships a newer model — council/conduct/auto also
# print a one-line hint when your config has fallen behind.
consilium models            # report the best live model per provider
consilium models --write    # rewrite consilium.config.json to adopt them

# Non-interactive setup (CI/scripts): write the recommended council without prompts.
consilium init --yes
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

Each verify command (and `auto --check`) is capped at `verify.timeoutSecs`
(default 600): a hanging test or build is killed at the cap and recorded as a
TIMEOUT-failed verify instead of stalling the run.

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
Recorded in the transcript per subtask as `status` + `summary`. (Subtasks form a
`depends_on` DAG executed in dependency-order waves — still one worker at a time;
a failed subtask isolates to its dependents, which are recorded `skipped`, instead
of aborting the whole run. True per-wave parallelism + git-worktree isolation is
deferred to Phase B.)

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
- **`review_diff`** — send a unified diff to the configured reviewer for a
  read-only audit; returns structured findings (`parse_ok:false` ⇒ unusable
  review, fail closed; `has_critical:true` ⇒ blocking). For a true cross-family
  check, configure a reviewer of a different family than the worker.
- **`council_run`** — convene the full council (workers answer independently,
  anonymized cross-review, chairman synthesis) from inside your session.
- **`search_recall`** — search your past run transcripts on disk for a term; returns matching run ids, tasks, and snippets (memory/recitation tools).
- **`page_in`** — load a compact, ~4000-char digest (task, outcome, per-subtask titles + summaries) of a past run transcript by id.

Register it in a Claude Code session (`.mcp.json` or `claude mcp add`):

```jsonc
{ "mcpServers": { "consilium": { "command": "consilium", "args": ["mcp"] } } }
```

Then ask your session to delegate: it decides *what* to hand off and whether to
accept; the engine executes. Logs go to stderr so they never corrupt the stdio
protocol.

### Or install the Claude Code plugin (one line)

The plugin bundles that MCP server plus slash commands and a skill, so your live
session knows how to conduct without any config:

```
/plugin marketplace add TemurTurayev/consilium
/plugin install consilium@consilium
```

You get:

- **`/consilium:conduct <task>`** — your session conducts: decompose → delegate to
  worker models → review their diffs.
- **`/consilium:council <question>`** — an anonymized multi-model second opinion.
- **`/consilium:review [path|staged]`** — a cross-family audit of your changes.
- a **skill** so Claude reaches for the council on its own when you ask for a
  second opinion or have build work worth offloading.

Prerequisite: the `consilium` CLI must be installed (see [Install](#install)) — the
plugin's MCP server runs `consilium mcp`. The commands detect a missing binary and
point you here.

## Live run streaming (`consilium serve`)

`consilium serve` starts a localhost server with a WebSocket at `/ws/session`.
Open it, send one frame describing the run, and receive every `AgentEvent` live
as it happens, then a terminal `run_complete` frame:

```bash
consilium serve --addr 127.0.0.1:7878
# then, from a WS client:
#   → {"kind":"conduct","task":"add a CHANGELOG"}   # cwd optional
#     (cwd defaults to — and must stay within — the dir you launched `serve` in; cross-origin connections are rejected)
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
`core/src/protocol.rs`), so `cargo test` regenerates them, and if a Rust change renames or removes a field the UI uses, the next `tsc` build catches it. The view's logic is a pure reducer (unit-tested with
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
same external scorer, only the grounding gate differs.

**First live run** (`--spend-quota`, 4 tasks, N=1): `solo`, `conduct`, and `conduct-no-grounding` all passed **4/4** — and `conduct` did it on **~76K Claude tokens vs solo's ~220K** (the build work offloads to Codex/Gemini). Same correctness, a third of the expensive Claude quota. N=1 can't separate *quality* (everything passed), but the token-cost delta is the headline — raise `--trials` for tighter numbers. See
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
$ consilium doctor
✓ claude   2.1.111 (Claude Code)
✓ codex    codex-cli 0.139.0
✓ agy      1.0.10
```

## How it works (M1)

```
 claude / codex / agy CLIs             (your subscriptions; agy=Antigravity→Gemini)
        │ headless stdout
        ▼
 adapters/        pure parsers: raw CLI output → normalized AgentEvent
        │         (fixture-tested against RECORDED REAL outputs)
        ▼
 sessions.rs      tokio process spawn, event streaming, stderr-drain
        │
        ▼
 AgentEvent       SessionStarted · Thinking · Message · ToolCall ·
        │         FileChanged · Usage · Completed · Failed
        ▼
 quota.rs         SQLite usage log, sliding-window aggregation
```

### Token semantics actually differ per provider

Claude and Codex are verified against recorded real CLI outputs (`core/tests/fixtures/{claude,codex}/recorded/`); Gemini via `agy` emits plain text, so its tokens are estimated; Grok is new and unverified against a real CLI, so its tokens are estimated too, for now:

| Provider | Input side | Output side |
|---|---|---|
| Claude | `input + cache_creation + cache_read` — cache tokens are **disjoint** additions | `output` |
| Codex | `input` only — `cached_input_tokens` is a **subset**, summing would double-count | `output + reasoning_output_tokens` |
| Gemini (via `agy`) | Antigravity's `agy` reports no usage envelope, so tokens are **estimated** (~4 chars/token, flagged `estimated` in the quota log) | estimated |
| Grok (via `grok`, **experimental / beta CLI**) | The Grok Build CLI's headless NDJSON schema is unverified against real output (it's brand new and its docs mark the event schema BETA) — the adapter parses defensively and emits `Usage` only when a line clearly carries token counts, so in practice tokens are **estimated** until real fixtures are recorded (`core/tests/fixtures/grok/recorded/`) | estimated |

## Development

- TDD throughout; `cargo test` (unit + integration), `cargo clippy --all-targets -- -D warnings`, `cargo fmt`.
- Adapter parsers are tested against **recorded real CLI outputs** committed as fixtures — format regressions surface without spending quota. Re-record with `script/record_fixtures.sh` (spends a few real requests).
- Design spec: [`docs/specs/`](docs/specs/) · implementation plans: [`docs/plans/`](docs/plans/)

## Roadmap

Shipped milestones are in [Status](#status). What's planned next:

**Smarter orchestration**
- **Parallel waves (fan-out Phase B)** — run a dependency wave's independent subtasks *concurrently*, each in an isolated git worktree. The sequential `depends_on` DAG already ships; this adds the speed.
- **Self-improving recommendations** — feed `consilium eval` results back into the provider catalog's per-role scores, so "best model per role" calibrates from real benchmarks instead of hand-tuned defaults.
- **Research-backed routing** (from Sakana's TRINITY / Conductor work) — task-type → preferred-family bias on top of least-loaded routing, cross-family review *on by default* for hard tasks, a configurable rework cap, and skipping the planning step on trivial tasks.

**Providers & onboarding**
- **Self-updating model pool** — live model discovery + a remotely-refreshable recommendations catalog, so weekly model releases appear without a Consilium update.
- **More providers** — Chinese frontier models (GLM / DeepSeek / Kimi) via the catalog + auth frame (CLI or OpenAI-compatible adapters — the same shape as the `agy` adapter).

**Distribution & platform**
- **Homebrew + checksummed installers** (via cargo-dist) on top of today's `curl | sh`.
- **Native Windows** — today via WSL; native needs the verify/spawn paths de-POSIX'd.
- **Web UI Council view**, **Warp** terminal integration (OSC 777), and a **Tauri** desktop app.

**Hardening** — prompt-injection delimiting on the model→model prompt channels, restricted-permission run transcripts, and a "trust this repo?" prompt before running config-declared verify commands.

Detail lives in [`docs/specs/`](docs/specs/) and [`docs/plans/`](docs/plans/); deferred items are each tracked.

## License

[MIT](LICENSE)
