# Consilium v1 — Design Spec

*Date: 2026-06-11 (rev 2: Rust core, attached conductor). Status: under product owner review.*
*Spec language: English (translated from the Russian original at repo publication, 2026-06-11).*

## 1. What it is

**Consilium** is an orchestrator that unites subscription-based CLI agents (Claude Code, Codex CLI, Gemini CLI) into a single "consilium": agents deliberate, anonymously review one another, and divide work so as not to burn through the limits of any single subscription.

The metaphor is a medical consultation board: specialists from different fields confer over a single patient. Tagline: *"Get a second opinion. And a third."*

MIT license, public GitHub repository.

## 2. Key decisions (locked)

| Decision | Choice | Rejected |
|---|---|---|
| v1 form | Standalone engine + local web UI | Fork of Warp (1.4M LOC, custom panels require core changes, spec-gated contributions, AGPL); plugin inside Claude Code only (limits UX) |
| Core language | **Rust** (tokio + axum), single binary | TypeScript core (rejected: Tauri desktop is Rust-native, Warp ecosystem is Rust/rmcp, single-binary distribution, GitHub audience trust) |
| Web UI | TypeScript + React; protocol types auto-generated from Rust (ts-rs) | Manual duplication of types |
| Model access | Official CLIs on user's subscriptions only | Direct API calls (defeats the point: subscription economics) |
| Conductor | **attached** by default: interactive Claude Code session via MCP (uses subscription); detached (`claude -p`, uses credit) — for autonomous runs | PTY automation of interactive CLI (grey area, exactly what Anthropic is closing off — not doing it) |
| Default mode | `auto` (pipeline composed from primitives) | Manual mode selection per task |
| After v1 | v1.1 Warp adapter (OSC 777), then Tauri desktop, expanded MCP surface | — |

## 3. Economic model (critical!)

**Update 2026-06-17:** A planned change to move `claude -p` / Agent SDK off the flat
subscription pool onto a separate monthly credit (Pro $20 / Max 5x $100 / Max 20x $200)
was **put on hold**. Headless Claude currently runs on flat subscription limits, same as
interactive — there is no separate credit to spend right now. Anthropic is reworking the
plan and will give advance notice. The design below treats the repricing as a *possible
future*, not a current fact; Consilium works either way. Codex CLI operates within ChatGPT
subscription limits (5-hour + weekly windows), Gemini CLI — daily quotas on the Google account.

Architectural implications:

1. **Attached mode is insurance, not a forced workaround**: the conductor *can* live in an interactive Claude Code session and drive the engine via MCP. Today this is mainly about richer context; if metered headless usage returns, it also keeps the conductor on flat subscription limits. Detached (`claude -p`) is currently just as economical.
2. **Claude is the low-frequency brain**: planning, arbitration, synthesis. Default routing does not send bulk code generation to it (Claude worker on Sonnet is a deliberate user choice in config, under quota-module control).
3. **Codex + Gemini are the workhorses**: draft generation, research, routine edits.
4. **The quota module tracks each pool separately**: Claude (subscription windows; or a programmatic-credit pool if the repricing returns), ChatGPT (windows), Gemini (daily quotas). Spend visibility is a headline product feature regardless.
5. Optional API-key fallback to a provider, but the default is subscriptions.

## 4. Architecture

```
consilium/
├── core/                   # engine — Rust (Cargo workspace)
│   └── src/
│       ├── adapters/       # claude.rs, codex.rs, gemini.rs + trait Adapter
│       ├── orchestrator/   # primitives: council.rs, conduct.rs, review.rs
│       │   └── auto.rs     # pipeline composition of primitives (default)
│       ├── supervisor.rs   # continuous observer
│       ├── sessions.rs     # spawn/lifecycle of agent processes (tokio::process)
│       ├── quota.rs        # pool accounting, load-based routing
│       ├── config.rs       # consilium.config.json (serde + validation)
│       ├── server/         # axum: REST + WebSocket on localhost
│       └── mcp.rs          # MCP server (rmcp) — attached conductor mode
├── web/                    # UI (React + Vite + Tailwind, TypeScript)
├── bindings/               # ts-rs: auto-generated TS protocol types
└── docs/
```

The engine is a single local binary (`consilium serve`); the UI is static files it serves.
No external servers, telemetry, or accounts. All data is stored locally in `~/.consilium/`
(SQLite via rusqlite: sessions, events, quota counters).

Core stack: tokio (async, processes), axum (HTTP/WS), serde/serde_json, rusqlite,
rmcp (the official Rust MCP SDK — the same one Warp uses), ts-rs (TS type generation).

### 4.1 Adapters

Each adapter launches the official CLI in headless mode and normalises its stream into
a unified `AgentEvent`:

| Provider | Invocation | Parsing |
|---|---|---|
| Claude (detached) | `claude -p --output-format stream-json --model <m>` | stream-json events |
| Codex | `codex exec --json -m <m>` | JSONL events |
| Gemini | `gemini -p <prompt> -m <m>` (+ JSON flags per CLI version) | stdout/JSON |

`AgentEvent` (Rust enum + serde, exported to TS via ts-rs): `SessionStarted`, `Thinking`,
`ToolCall`, `FileChanged`, `Message`, `Usage` (tokens), `Completed`, `Failed`.

Principles: the CLI version is detected at startup (`--version`), format breaks are isolated
inside the adapter; fixture tests on recorded real CLI output catch format regressions without
spending any limits. A new provider = one new adapter file implementing the trait.

### 4.2 Orchestration primitives

**`council`** (Karpathy's pattern ported to coding CLIs):
1. The question is sent to N agents independently (in parallel).
2. Responses are anonymised (Agent A/B/C) — models do not know whose work they are reviewing.
3. Each agent reviews and ranks the others' responses against given criteria.
4. A chairman (role `chairman`) synthesises the final decision.
Use case: architectural decisions, complex bugs, approach selection.

**`conduct`** (conductor):
1. The `conductor` role decomposes the task into self-contained subtasks (each prompt
   contains all necessary context — workers cannot see other sessions or history).
2. Subtasks are distributed to workers factoring in quota and specialisation.
3. Workers operate in parallel, each in their own session in the project working directory.
4. The conductor evaluates results: accept / send back for rework (with specifics) / integrate.
Rule: workers never communicate directly; all context flows through the conductor.

**`review`** (cross-review):
a diff from one agent is sent to another for audit (bugs, security, quality);
in case of disagreement — an arbitrator (role `chairman`). The cheapest everyday mode.

### 4.3 `auto` mode (default)

A state machine that composes primitives by phase:

```
task → [PLANNING]    council: plan/approach (if task is non-trivial)
     → [EXECUTION]   conduct: distribute subtasks to workers
     →               review: each completed subtask
     → [INTEGRATION] conductor assembles, runs tests
     → [DONE]        report to user
  (supervisor observes continuously across all phases)
```

Complexity triage is performed by the conductor on its very first call: a trivial task
skips PLANNING and goes to a single worker (saving costly calls). The user can
force a specific primitive manually (`/council ...`, `/review ...`).

### 4.4 Conductor modes (subscription economics)

**attached (default for interactive work).** The conductor is the user's live interactive
Claude Code session. The engine (`consilium serve`) exposes an MCP server
(`consilium mcp`, stdio or streamable HTTP) with tools: `council_run`,
`delegate_task`, `review_diff`, `worker_status`, `quota_status`. The Claude Code session
makes decisions (subscription, flat limits), the engine executes — running Codex/Gemini
headless and streaming events to the web UI. Claude's programmatic credit is not spent.

**detached (autonomous).** Fully background run with no open session: Claude roles
via `claude -p` — spend programmatic credit. For overnight runs, CI scenarios,
launching from the web UI without an open Claude Code session.

The web UI works in both modes; in attached mode it mirrors activity and allows
worker management. The engine tags each Claude session with its mode — the quota module
attributes spend to the correct pool.

### 4.5 Supervisor ("infrequent but precise")

A dedicated observer role. Economics: reading (input) is vastly cheaper than generation
(output), so the supervisor **reads a lot, writes little**.

- Subscribed to event streams of all active sessions; maintains a rolling summary of activity.
- Default provider — **Gemini** (enormous context window, most generous quotas):
  can hold the state of all agents in mind simultaneously.
- Intervention triggers: a worker is stuck in a loop (repeating the same actions), drifting
  from the subtask spec, editing files outside scope, repeated test failures, silence beyond a threshold.
- Intervention: a short corrective note injected into the worker's session + a flag in the UI.
  For a serious conflict — escalation to the conductor.
- Sensitivity is configurable (`interventionThreshold: low|medium|high`).

### 4.6 Roles and config

`consilium.config.json` (serde validation, sensible out-of-the-box defaults):

```jsonc
{
  "roles": {
    "conductor":  { "provider": "claude", "model": "fable-5", "effort": "high",
                    "mode": "attached" },
    "chairman":   { "provider": "claude", "model": "fable-5", "effort": "high" },
    "workers": [
      { "provider": "codex",  "model": "gpt-5.4" },
      { "provider": "gemini", "model": "gemini-3-pro" },
      { "provider": "claude", "model": "sonnet" }   // for regular coding
    ],
    "reviewer":   { "provider": "codex",  "model": "gpt-5.4" },
    "supervisor": { "provider": "gemini", "model": "gemini-3-pro",
                    "interventionThreshold": "medium" }
  },
  "quota": {
    "claude":  { "programmaticCreditUsd": 100 },   // Max 5x
    "gemini":  { "dailyRequests": 1000 },
    "codex":   {}                                   // windows auto-detected from errors
  }
}
```

Any role can use any provider/model: "decision-making" roles get the top model with high
effort, routine work gets cheaper models. Effort is translated into the corresponding CLI flags.

### 4.7 Quota and routing

- Counters from `Usage` events emitted by adapters are written to SQLite; windows are rolling.
- For Claude programmatic: token-to-dollar conversion at the public API pricing
  (configurable price table) → remaining credit is visible.
- Worker routing: score = available quota × suitability for task type;
  when a pool is exhausted the provider is excluded until the window resets, the UI warns.

## 5. Web UI (localhost)

Three screens:

1. **Dashboard** — agent cards (status: working/waiting/idle, current subtask),
   health indicators for all four quota pools, supervisor intervention feed.
2. **Session** — live log of the selected session (events, tool calls), diff viewer.
3. **Council** — tabs of independent responses, cross-rating matrix,
   chairman's final synthesis. The product showcase (GIF for README).

WS protocol: UI subscribes to channels `session:<id>`, `quota`, `supervisor`.
Dark theme by default.

## 6. Testing

- Core: `cargo test`, TDD; adapters — fixture tests on recorded real CLI output
  (`fixtures/<provider>/<version>/*.jsonl`) — format regression coverage without spending limits.
- Orchestrator: unit tests with a mock adapter (trait!) — happy path, worker failure,
  quota exhaustion, supervisor intervention.
- Web: vitest on components, Playwright smoke tests against the engine with mock adapters.
- CI: GitHub Actions (cargo fmt + clippy + test; web lint + build + test) on every PR.

## 7. Out of scope for v1

Warp adapter (v1.1; OSC 777 protocol studied), Tauri desktop, git worktree isolation
for workers (v1: shared working directory, sequential integration by conductor;
worktrees — v1.2), team/cloud features, telemetry.

## 8. Risks

| Risk | Mitigation |
|---|---|
| Headless CLI output formats change | Isolation in adapters + fixture tests + version detection |
| Rust has a slower iteration speed than TS | The bulk of code is written by Claude; the compiler catches whole classes of bugs before runtime; mock-trait simplifies tests |
| $100/month Claude credit is insufficient | Attached mode by default (credit not spent); quota module shows remaining balance; configurable budget stop |
| Vendor tightens headless policy | Attached mode already conforms to the spirit of the policy; adapters abstract the invocation method; API-key fallback |
| Parallel workers conflict over files | v1: conductor issues non-overlapping scopes; worktrees in v1.2 |
| `codex` CLI not installed on user's machine | `consilium doctor`: checks/installs/logs in all three CLIs |

## 9. Definition of Done for v1

1. `cargo install consilium` (and a Homebrew formula) + `consilium serve` brings up the engine and UI
   on a clean machine; `consilium doctor` verifies/repairs the presence and login state of all three CLIs.
2. **Attached mode**: from an interactive Claude Code session, `council_run`
   and `delegate_task` are invoked via MCP — work runs on the subscription, programmatic credit is not spent
   (visible in the quota panel).
3. `auto` mode completes a real task at the level of "add a feature to a small repository"
   end-to-end with all three providers visibly working.
4. `council` produces a synthesised answer with a rating matrix on a real question.
5. Quotas for all pools are visible and update in real time.
6. Tests are green (cargo + web), CI is configured.
7. README (EN) with a Council-screen GIF + Quick Start.
