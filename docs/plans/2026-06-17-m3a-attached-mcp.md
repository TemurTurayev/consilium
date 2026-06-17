# M3a — Attached-conductor MCP MVP

> Status: IMPLEMENTED. First slice of M3 (Server & UI). Scoped by an
> understand+decide workflow; the rmcp API was de-risked by a compile spike
> before writing this. M3 slices: **M3a (this)** → M3b axum+WebSocket streaming →
> M3c full 5-tool surface + cross-family review → M3d memory tools + recitation →
> M3e ts-rs + React UI.

## Why this is M3's first slice

It de-risks the single most uncertain *and* highest-value idea in the milestone:
that the **attached-conductor-via-MCP** pattern works against the real CLIs and
keeps programmatic Claude credit off the clock (spec §2/§4.4 call attached "the
default"; README calls it "the key to subscription economics"). Everything else
in M3 (axum, WebSocket, React, dashboards) is conventional plumbing. M3a is ~2
tools over existing library functions — if the inversion had a fatal flaw (rmcp
integration, the advisory/write guard not transferring, `run_with_failover` not
composing as a one-shot tool), we learn it before building a UI on top.

## What shipped

A `consilium mcp` subcommand running an **rmcp v1.7 stdio MCP server**
(`core/src/mcp.rs`) exposing exactly two tools, both thin wrappers over existing
library functions — zero changes to the orchestration loops:

- **`run_worker(prompt, worker_label, cwd, timeout_secs?)`** → resolves the named
  worker's failover ladder, runs it via `run_with_failover`
  (**`advisory:false, write:true`**, never exposed as a param), then
  `capture_changes` + the configured `verify` (P0 #1 grounding). Returns
  `{ ok, model_used, worker_report, changes, verify, error }`; all-rungs-fail
  returns a structured `error`, never panics.
- **`quota_status()`** → per-provider `(input, output)` tokens over the 5h window.

`McpServer` resolves workers once at construction (`new`, mirroring
`ConductDeps`) and has a test seam (`from_parts`) that injects scripted ladders.

## Key decisions / changes beyond the two tools

- **`QuotaStore` made `Sync`** (wrapped its rusqlite `Connection` in a `Mutex`,
  `&self` signatures unchanged). Required: rmcp tool futures must be `Send`, and
  holding `&QuotaStore` across the worker `await` needs `QuotaStore: Sync`. This
  is the migration the scope flagged; benign for all existing callers.
- **Logs → stderr** (`tracing_subscriber ... with_writer(stderr)`). The MCP
  server owns stdout for JSON-RPC; any log on stdout corrupts the framing.
- **Security invariant transferred**: `run_worker` exposes no `advisory` knob, so
  the deliberation-grade trust relaxation can never combine with auto-approved
  writes at the tool boundary (mirrors `sessions.rs`). Pinned by a test asserting
  the worker was invoked with `advisory:false, write:true`.
- **Deferred to later M3 slices**: the other three tools (`council_run`,
  `review_diff` with cross-family routing, `worker_status`), the WebSocket
  server, ts-rs, and the UI. `worker_label` selection is the seam where
  cross-family review (research Finding 7) lands in M3c.

## Tests (`core/tests/mcp_test.rs`, zero quota)

- `run_worker_routes_writes_captures_and_uses_scoped_flags` — scripted worker
  writes a file; asserts `ok`, `model_used`, captured changes, and (via
  `RecordingAdapter`) that the worker ran `advisory:false, write:true`.
- `run_worker_unknown_label_returns_structured_error` — bad label → `ok:false`,
  error lists known workers.
- `run_worker_runs_the_configured_verifier` — verify config runs and reports
  `passed` (grounding wired through the tool).
- `quota_status_reports_recorded_totals` — totals per provider over the window.

## Verification

- `cargo test` green; `cargo clippy --all-targets -- -D warnings` exit 0;
  `cargo fmt --check`.
- **Live stdio smoke**: drove the real `consilium mcp` server with a JSON-RPC
  `initialize` → `tools/list` → `tools/call quota_status` sequence; got both tool
  schemas and real usage totals from `~/.consilium/usage.db`, `isError:false`,
  clean EOF exit, stdout uncorrupted.
- Whole-branch adversarial review before merge.

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| rmcp SDK maturity | de-risked by a compile spike + a live stdio smoke; pinned `rmcp = 1.7`, `schemars = 1.2` (unified with rmcp's) |
| advisory/write guard not transferring | no `advisory` param on `run_worker`; test pins `advisory:false, write:true` |
| QuotaStore `!Sync` blocked `Send` futures | wrapped `Connection` in a `Mutex` (now `Sync`); existing tests still pass |
| logs corrupting stdio JSON-RPC | tracing writes to stderr |
| concurrent tool calls | `Arc<QuotaStore>` + internal mutex serializes; fine for the attached MVP |
