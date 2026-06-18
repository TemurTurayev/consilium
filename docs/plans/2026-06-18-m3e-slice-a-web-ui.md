# M3e (Slice A) — Live web UI over the M3b WebSocket server

> Status: IMPLEMENTED. Added beyond this plan during build: a **zero-backend
> demo mode** (`Demo run` replays a canned council session through the same
> reducer) — strengthens the showcase + the live proof without quota. M3e is the
> visible showcase: a browser that streams a
> `conduct` run live. M3b (the axum `/ws/session` server) already exists and has
> no client but the test — this slice is its first real consumer. Scoped into
> Slice A (the live **Session** view, end-to-end) and later slices (Dashboard /
> Council views, single-binary packaging).

## Why now

The streaming backend (M3b) is consumer-less. The smartest payoff from the
multi-provider engine is *visible deliberation* — a human watching the council
think, call tools, change files, and converge. Slice A makes that real with the
smallest surface that proves the whole pipe: typed Rust protocol → ts-rs bindings
→ React reducer → live timeline.

## Decisions (settled — do not relitigate)

1. **Stack: Vite 7 + React 19 + TypeScript 5.7 + Vitest 2.1**, in a new `ui/`
   folder at the repo root. Rejected vanilla HTML/ES-modules: the whole elegance
   of this slice is a *typechecked* single-source-of-truth contract, which only
   pays off with a TS frontend; React also scales to the deferred Dashboard /
   Council views without a rewrite. Node 25 / npm 11 present.
2. **Protocol typing: two sibling tagged enums, not one mega-enum.** `AgentEvent`
   (engine-owned, serialized verbatim by `WsSink`) stays as-is; a new
   `ServerFrame` enum (server-owned lifecycle frames) replaces the three ad-hoc
   `serde_json::json!` sites in `server.rs`. They share only the `type`
   discriminant *namespace* on the wire (tags are disjoint). Folding AgentEvent
   into ServerFrame would force `WsSink` to wrap/nest every event — worse. The TS
   client unions them: `type InboundFrame = AgentEvent | ServerFrame`.
3. **ts-rs export-via-test**, `#[ts(export, export_to = "../../ui/src/protocol/")]`
   on every protocol type. Path is relative to the **source file's dir**
   (`core/src/`) — verified by spike. `cargo test` regenerates the `.ts`; the
   files are **committed** (UI must build in CI without cargo; PRs show contract
   diffs). u64 fields carry `#[ts(type = "number")]` (serde emits a bare JSON
   number; ts-rs's default `bigint` would be a type lie — verified fixed).
4. **Dev model: two processes** (`consilium serve` + `npm run dev`), WS URL from
   `VITE_WS_URL` (default `ws://localhost:7878/ws/session`). Raw WS has no CORS
   preflight, so no proxy needed. Embedding the built UI into the binary
   (single-origin, one-click) is **deferred** to a later slice — it couples the
   Rust release to a Node build.
5. **`conduct` only.** Council/auto WS kinds are blocked upstream anyway
   (tokio-spawned council members don't inherit the task-local `ProgressSink`);
   Slice A streams the one kind that runs inside the sink scope.

## Verified spike facts (ts-rs 12.0.1)

- `serde-compat` (default) honors existing `#[serde(tag=..., rename_all=...)]` —
  no re-declaration. `Provider` → `"claude" | "codex" | "gemini"`. `AgentEvent` →
  discriminated union with snake_case `"type"`. `Option<String>` → `string | null`.
- Export tests `export_bindings_<type>` run under `cargo test`; overwrite the
  `.ts`. MSRV 1.78 (have 1.96).

## Wire contract (the seam)

- **Live frames** = `AgentEvent` (`session_started|thinking|message|tool_call|file_changed|usage|completed|failed`).
- **Control frames** = `ServerFrame`:
  - `run_complete { completed: number[], halted: string|null, failed: string|null }`
    (real `ConductOutcome` shape — `completed: Vec<u32>`, `halted/failed: Option<String>`).
  - `run_error { error: string }`
  - `error { error: string }` (first-frame parse rejection)
- **Request** = `SessionRequest::Conduct { task, context, cwd }` (`{"kind":"conduct",...}`).

## File manifest

**Rust — create:**
- `core/src/protocol.rs` — `ServerFrame` enum + `SessionRequest` (moved from
  server.rs), both `#[derive(TS)]`; `From<&ConductOutcome> for ServerFrame`
  helper; unit tests asserting exact serialized tags/fields + `decl()` invariants
  (u64→number, nullable halted/failed).

**Rust — modify:**
- `core/src/event.rs` — TS derives on `Provider` + `AgentEvent` (done in spike).
- `core/src/lib.rs` — `pub mod protocol;`.
- `core/src/server.rs` — import `protocol::{ServerFrame, SessionRequest}`; delete
  the in-file `SessionRequest`; replace the 3 `json!` frame sites with
  `serde_json::to_string(&ServerFrame::…)`. Keep `WsSink` + the 3 unit tests.
- `core/Cargo.toml` — `ts-rs = "12"` (done in spike).
- `README.md` — note the browser UI under live streaming.

**Frontend — create (`ui/`):**
- `package.json`, `tsconfig.json`, `tsconfig.node.json`, `vite.config.ts`,
  `index.html`, `.env.example`, `.gitignore`, `README.md`.
- `src/main.tsx`, `src/App.tsx`, `src/index.css`.
- `src/protocol/` — `Provider.ts` `AgentEvent.ts` `SessionRequest.ts`
  `ServerFrame.ts` (ts-rs generated, committed) + `index.ts` (hand-written barrel:
  `InboundFrame`, type guards).
- `src/session/` — `wsUrl.ts`, `parseFrame.ts` (+`.test.ts`), `reducer.ts`
  (+`.test.ts`), `useSession.ts`.
- `src/components/` — `StartRunForm.tsx`, `SessionHeader.tsx`, `EventStream.tsx`,
  `EventRow.tsx`, `UsageBadge.tsx`, `ResultPanel.tsx`.

## Architecture (frontend)

- **Pure core (unit-tested, no React/socket/clock):**
  - `parseFrame(raw: string): { ok: true; frame: InboundFrame } | { ok: false; raw: string }`
  - `sessionReducer(state, action)` + `initialState`. Actions translate socket
    lifecycle + parsed frames; folding rule discriminates every inbound frame on
    `type`. `usage` accumulates; `file_changed` dedups; AgentEvents append to a
    timeline; `run_complete`→done, `run_error`/`error`→errored.
- **Impure shell:** `useSession()` owns the `WebSocket`, dispatches parsed frames
  into `useReducer`, exposes `{ state, status, start(req) }`.
- **Presentation:** dumb components driven by reducer state; `EventRow` switches
  on `event.type` with a default "unknown event" row (forward-compat with future
  Rust variants).

## Aesthetic

Dark, calm "operating-theatre at night". `index.css` custom properties; provider
accents claude `#d97757` / codex `#10a37f` / gemini `#4285f4`. Centered ~820px
column, 3px provider accent rail per row, monospace type-badges, tinted terminal
banners (ok/warn/err), status pill. `prefers-reduced-motion` respected. No UI
framework, no web fonts (offline localhost tool).

## Tests

- **Rust:** existing 3 `server_test.rs` pass unchanged (typed frames serialize
  byte-identically — substring asserts hold). New `protocol.rs` unit tests:
  `ServerFrame` round-trips with exact tags/fields; `decl()` contains
  `input_tokens: number`, nullable `halted`/`failed`. Existing 152 lib + others
  unaffected.
- **Frontend (Vitest):** `reducer.test.ts` — every AgentEvent variant folds;
  usage accumulates across frames; control frames set terminal/error; a recorded
  full-session replay reproduces aggregate state. `parseFrame.test.ts` — valid
  live/control JSON, malformed JSON, unknown `type` tag survives.

## Verification (done-condition)

- `cargo test` green (incl. regenerated bindings + new protocol tests); `cargo
  clippy --all-targets -- -D warnings` exit 0; `cargo fmt --check`.
- `cd ui && npm install && npm run test` green; `npm run build` (tsc typechecks
  the React app against the **generated** bindings — proves contract integrity end
  to end).
- Live render proof: `npm run dev` + screenshot of the rendered Session view.
- The seam argument: backend WS test (server→wire) + reducer replay (wire→UI) +
  shared drift-guarded bindings (both sides agree) = end-to-end without quota.

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| Contract drift (Rust ↔ TS) | `cargo test` regenerates + commits bindings; `npm run build` typechecks against them |
| u64 → bigint type lie | `#[ts(type="number")]` (verified → `number`); token counts ≪ 2^53 |
| Blast on 3 server tests | typed frames preserve exact tags/field names → byte-identical JSON (verified field types) |
| WS drop mid-run looks like a stall | surface `onclose`/`onerror` as explicit status; no auto-reconnect (deferred) |
| Beginner maintainer | two npm commands; logic in a plain pure reducer; ts-rs runs inside `cargo test` they already use |

## Deferred (later M3e slices)

Quota dashboard; Council/auto WS kinds (need engine change for sink inheritance);
past-runs history; single-binary static serving (rust-embed/ServeDir + `--open`);
WS reconnect/resume; auth; Playwright E2E (needs a stub WS harness).
