# M3b — Live run streaming (ProgressSink seam + axum WebSocket server)

> Status: IMPLEMENTED. Second slice of M3. Scoped by an understand+decide
> workflow. Originally planned as two PRs (M3b1 engine seam + M3b2 server); the
> task-local seam turned out so small/low-risk that both shipped on one branch,
> with M3b1 committed first as a checkpoint.

## The blocker it removes

The orchestrator buffered all `AgentEvent`s and returned only a final outcome
(`run_to_completion` collects into a Vec). A browser/dashboard needs them **live**.

## Seam (M3b1): a task-local `ProgressSink`, not threaded params

`orchestrator::progress`: a 1-method trait `ProgressSink::on_event(&AgentEvent)`,
a `tokio::task_local! { PROGRESS_SINK: Arc<dyn ProgressSink> }`, and `emit(&ev)`.
`run_to_completion` calls `progress::emit(&ev)` inside its existing event-collect
loop. A server installs a sink via `PROGRESS_SINK.scope(sink, run_future)`;
everywhere else (CLI, all tests) no sink is in scope, so `emit` is a no-op.

**Why task-local over threading `Option<&dyn ProgressSink>`** through
`run_to_completion`/`run_with_failover`/`run_conduct`/`council`/`auto`: the sink
is *ambient run context*, not data the engine computes with. The read happens
inline in the same task as the run (no `tokio::spawn` between the server's
`scope` and the collect loop), so the task-local is visible exactly where events
arrive — with **zero orchestration-signature changes and zero test churn** (the
206 tests stayed byte-identical). The understand-phase readers proposed threading
a broadcast `Sender`; that would have touched ~16 call sites + 2 integration
tests. (Caveat: parallel council members, if `tokio::spawn`ed, would not inherit
the task-local — out of scope for the conduct-first server; council/auto stream
later.)

## Server (M3b2): one WebSocket endpoint

New `core/src/server.rs` + `consilium serve --addr --timeout`:
- `GET /ws/session`: client sends one frame
  `{"kind":"conduct","task":"...","context":"...","cwd":"..."}`; the handler
  splits the socket, builds a `WsSink` that forwards each event (as JSON) into an
  unbounded channel, spawns a writer task draining it to the socket, and runs
  `run_conduct` inside `PROGRESS_SINK.scope(sink, ...)`. On completion it sends a
  terminal `run_complete`/`run_error` frame, drops the senders, and the writer
  closes the socket.
- `WsSink::on_event` is non-blocking (a channel send), so the engine's collect
  loop never stalls on socket backpressure.
- `ServerState` holds a `deps_fn: Arc<dyn Fn() -> ConductDeps>` — production
  resolves from config; tests inject scripted ladders (mirrors M3a `from_parts`),
  enabling a deterministic zero-quota E2E.
- New deps: `axum 0.8` (ws), `futures-util` (socket split); `tokio-tungstenite`
  (dev-only, the test WS client).

## Deferred (later M3 slices)

council/auto WS kinds; `quota` + `supervisor` channels; past-runs REST over the
transcript store; static UI serving + the React frontend (M3e); session-id
multiplexing/reconnect; killing the orphaned child on client disconnect.

## Tests (zero quota)

- `runner_test::progress_sink_in_scope_receives_every_event` — a scoped fake sink
  sees exactly the events the run collected (the M3b1 seam).
- `server.rs` units — `WsSink` forwards serialized events; `SessionRequest`
  parses `conduct` and rejects unknown kinds.
- `server_test::ws_streams_conduct_events_then_terminal_frame` — **E2E**: bind the
  router on a random port, connect a real WS client, send a conduct frame, assert
  live `AgentEvent` frames arrive before a `run_complete` frame with
  `completed:[1]`, and the worker's file was written — all on scripted adapters.

## Verification

- `cargo test` green (211, +5); `cargo clippy --all-targets -- -D warnings` exit
  0; `cargo fmt --check`. 206 pre-existing tests byte-unchanged.
- Whole-branch adversarial review before merge.

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| Cross-cutting hot-loop edit regressing the 206 tests | task-local seam → zero signature changes; full suite green unchanged |
| Slow WS client stalling the engine | `WsSink` is a non-blocking channel send; a writer task owns socket backpressure |
| task-local not visible past `tokio::spawn` | conduct is sequential/inline; documented; council/auto deferred |
| New dep surface (axum/tower) | isolated to `server.rs` + the `serve` arm; lib/MCP build unaffected |
| Orphaned child on client disconnect | pre-existing M1 timeout-orphan policy stands; deferred |
