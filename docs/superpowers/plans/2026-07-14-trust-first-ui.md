# Trust-First Onboarding and Review UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the opaque first-run form with a three-action, trust-first flow that previews repository safety, runs in an isolated worktree by default, and ends at an explicit diff review with Apply or Discard.

**Architecture:** The Rust server exposes short-lived prepared preflights and immutable result resources over same-origin authenticated HTTP, while WebSocket frames carry only live progress and result identifiers. The React UI owns a separate run-flow state machine for setup, confirmation, live execution, and review; the existing Table remains a visual lens over the same session rather than a second entry point. Desktop workspace selection goes through the Tauri command that restarts the embedded server, while web mode uses the server launch root.

**Tech Stack:** Rust 2021, Axum 0.8, Tokio, Serde/ts-rs, Tauri 2, React 19, TypeScript 6, Vite 8, Vitest 4, Testing Library, CSS Modules/current design tokens.

## Global Constraints

- This plan consumes the completed interfaces from `2026-07-14-trust-safety-core.md`; do not duplicate safety policy in React.
- Keep the product name **Consilium**, the medical Table scene, existing colors, and existing live session events.
- Primary actions are exactly **Build**, **Ask Council**, and **Review Changes**.
- Build is write-capable and defaults to a safe detached worktree; Ask Council and Review Changes are read-only.
- Preflight must show canonical path, Git/dirty state, execution mode, roles/providers, exact verification commands with source, timeout, budget, and warnings.
- Provider liveness probes are explicit because they may spend a small amount of quota; never run them automatically on page load.
- Desktop folder selection must call `pick_workspace`, persist the folder, restart the embedded server, and invalidate cached preflight state.
- Web mode cannot change the server launch root; explain this instead of showing a nonfunctional picker.
- Non-Git and dirty-repository limitations use plain language and never imply fake isolation.
- Apply and Discard are available only for `ResultState::Ready`; mutation endpoints are same-origin and require the per-server token.
- A source checkout changed after the run produces a preserved, recoverable result with Apply disabled.
- Demo mode requires no backend and no provider quota, but demonstrates setup → preflight → running → result review → discard.
- Keep `SessionRequest::Conduct` accepted for one compatibility release; new UI uses `SessionRequest::Start`.
- Do not place full patches or binary payloads on WebSocket frames.
- No Windows-native promise in this release.

---

## File Structure

- `core/src/protocol.rs`: shared HTTP/WS DTOs and generated TypeScript bindings.
- `core/src/server/safety_service.rs`: prepared-preflight/result registry, per-repository lock, and one-way result transitions.
- `core/src/server.rs`: HTTP routes, token/origin middleware, and WebSocket lifecycle mapping.
- `desktop/src-tauri/src/commands.rs`: authoritative native workspace picker/restart command.
- `ui/src/runtime.ts`: web/Tauri runtime adapter and session token bootstrap.
- `ui/src/runFlow/`: reducer, API client, action model, and controller hook.
- `ui/src/components/run/`: setup, preflight, live, and result-review surfaces.
- `ui/src/demoScenario.ts`: a complete quota-free safety-flow fixture.
- Existing `App.tsx` and Table consume one shared run flow.

### Task 1: Additive safety protocol and generated TypeScript bindings

**Files:**
- Modify: `core/src/protocol.rs`
- Modify: `ui/src/protocol/index.ts`
- Generate/update: `ui/src/protocol/*.ts`

**Interfaces:**
- Consumes: `ExecutionMode`, `SafetyPreflightReport`, `PreflightAcceptance`, `ResultState`, and result summary types from the core safety plan.
- Produces: `ProductAction`, `PreflightHttpRequest`, `PreparedPreflight`, `ResultSummary`, `SessionRequest::Start`, `ServerFrame::ResultReady`, and `ServerFrame::SessionComplete`.

- [ ] **Step 1: Write failing exact-wire tests**

```rust
#[test]
fn prepared_start_and_result_ready_have_stable_wire_shapes() {
    let start = serde_json::from_str::<SessionRequest>(
        r#"{"kind":"start","preflight_id":"pf-1","acceptance":{"command_digest":"abc","in_place_acknowledged":false}}"#,
    ).unwrap();
    assert!(matches!(start, SessionRequest::Start { preflight_id, .. } if preflight_id == "pf-1"));

    let frame = ServerFrame::ResultReady { result_id: "result-1".into() };
    assert_eq!(serde_json::to_string(&frame).unwrap(), r#"{"type":"result_ready","result_id":"result-1"}"#);
}

#[test]
fn product_actions_serialize_as_ui_labels_expect() {
    assert_eq!(serde_json::to_string(&ProductAction::Build).unwrap(), "\"build\"");
    assert_eq!(serde_json::to_string(&ProductAction::AskCouncil).unwrap(), "\"ask_council\"");
    assert_eq!(serde_json::to_string(&ProductAction::ReviewChanges).unwrap(), "\"review_changes\"");
}
```

- [ ] **Step 2: Run and confirm the new protocol variants are absent**

Run: `cargo test -p consilium protocol::tests::prepared_start_and_result_ready_have_stable_wire_shapes`

Expected: FAIL with unresolved variants/types.

- [ ] **Step 3: Add the exact additive DTOs**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub enum ProductAction { Build, AskCouncil, ReviewChanges }

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct PreflightHttpRequest {
    pub action: ProductAction,
    pub task: String,
    pub context: String,
    pub cwd: Option<String>,
    pub execution_mode: ExecutionMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct PreparedPreflight {
    pub id: String,
    pub report: SafetyPreflightReport,
    #[ts(type = "number")]
    pub expires_at_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct ChangedFileSummary {
    pub path: String,
    pub status: String,
    pub binary: bool,
    #[ts(type = "number | null")]
    pub size_bytes: Option<u64>,
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct VerificationRecordSummary {
    pub label: String,
    pub command: String,
    pub passed: bool,
    #[ts(type = "number")]
    pub duration_ms: u64,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct ProviderAttributionSummary {
    pub role: String,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "../../ui/src/protocol/")]
pub struct ResultSummary {
    pub id: String,
    pub state: ResultState,
    pub base_commit: String,
    pub source_repo: String,
    pub changed_files: Vec<ChangedFileSummary>,
    pub verification: Vec<VerificationRecordSummary>,
    pub attribution: Vec<ProviderAttributionSummary>,
    pub warning: Option<String>,
}
```

Add `SessionRequest::Start { preflight_id: String, acceptance: PreflightAcceptance }` and `ServerFrame::{ResultReady { result_id: String }, SessionComplete { result_id: Option<String> }, PreflightInvalidated { reason: String }}`. Keep the old `Conduct` and `RunComplete` variants unchanged for compatibility.

- [ ] **Step 4: Regenerate and typecheck bindings**

Run: `cargo test -p consilium protocol && npm --prefix ui run typecheck`

Expected: PASS; committed generated bindings contain all new types, while existing imports still compile.

- [ ] **Step 5: Commit**

```bash
git add core/src/protocol.rs ui/src/protocol
git commit -m "feat: define trust-first run protocol"
```

### Task 2: Prepared-preflight and result HTTP service

**Files:**
- Create: `core/src/server/safety_service.rs`
- Create: `core/src/server/mod.rs`
- Modify: `core/src/server.rs` or move its contents to `core/src/server/mod.rs`
- Modify: `core/src/lib.rs`
- Modify: `core/tests/server_test.rs`

**Interfaces:**
- Consumes: `inspect`, `prepare_write_run`, `ResultBundle`, `apply_result`, `discard_result`, and Task 1 DTOs.
- Produces: `SafetyService::{prepare,get_result,get_diff,apply,discard,take_preflight}`, plus routes `POST /api/preflight`, `GET /api/results/{id}`, `GET /api/results/{id}/diff`, `POST /api/results/{id}/apply`, and `POST /api/results/{id}/discard`.

- [ ] **Step 1: Write failing HTTP tests with an injected fake runner**

```rust
#[tokio::test]
async fn preflight_is_local_deterministic_and_mutations_require_token() {
    let fixture = ServerFixture::new().await;
    let response = fixture.post_json("/api/preflight", serde_json::json!({
        "action": "build", "task": "change base", "context": "", "cwd": null, "execution_mode": "safe_worktree"
    })).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = fixture.post_json_with_token("/api/preflight", serde_json::json!({
        "action": "build", "task": "change base", "context": "", "cwd": null, "execution_mode": "safe_worktree"
    })).await;
    assert_eq!(response.status(), StatusCode::OK);
    let prepared: PreparedPreflight = response.json().await.unwrap();
    assert!(!prepared.report.provider_probe_performed);
}

#[tokio::test]
async fn result_transition_is_one_way_and_stale_apply_is_preserved() {
    let fixture = ServerFixture::with_ready_result().await;
    fixture.dirty_source_checkout();
    let response = fixture.post_with_token("/api/results/result-1/apply").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let summary: ResultSummary = fixture.get("/api/results/result-1").await.json().await.unwrap();
    assert_eq!(summary.state, ResultState::Ready);
    assert!(summary.warning.unwrap().contains("changed"));
}
```

- [ ] **Step 2: Run and confirm the routes return 404**

Run: `cargo test -p consilium --test server_test preflight_is_local_deterministic_and_mutations_require_token`

Expected: FAIL with `404 Not Found`.

- [ ] **Step 3: Implement an injectable service and expiring prepared records**

```rust
pub trait SafetyRunner: Send + Sync + 'static {
    fn inspect(&self, request: &PreflightHttpRequest, launch_root: &Path) -> anyhow::Result<SafetyPreflightReport>;
    fn apply(&self, bundle: &ResultBundle) -> anyhow::Result<ResultBundle>;
    fn discard(&self, bundle: &ResultBundle) -> anyhow::Result<ResultBundle>;
}

pub struct SafetyService<R: SafetyRunner> {
    runner: Arc<R>,
    preflights: Mutex<HashMap<String, PreparedRecord>>,
    results: Mutex<HashMap<String, ResultBundle>>,
    repo_locks: Mutex<HashSet<PathBuf>>,
    ttl: Duration,
}

impl<R: SafetyRunner> SafetyService<R> {
    pub fn take_preflight(&self, id: &str) -> anyhow::Result<PreparedRecord> {
        let record = self.preflights.lock().unwrap().remove(id).context("preflight not found or already used")?;
        anyhow::ensure!(record.created.elapsed() <= self.ttl, "preflight expired");
        Ok(record)
    }
}
```

Prepared records are single-use, expire after 10 minutes, and bind action/task/context/canonical repo/mode/digest. Per-repository locks prevent two active write runs or simultaneous Apply/Discard transitions.

- [ ] **Step 4: Add same-origin and token protection to every mutation**

Generate a 256-bit random token at server startup. Expose it only through same-origin `GET /api/session-token`; require `X-Consilium-Token` and an allowed `Origin` on preflight, every result/diff read, Apply, and Discard. Keep existing nonsensitive version/config endpoints compatible, but apply the same origin allowlist. Return structured JSON `{ "error": "..." }` with 401, 409, or 422; never expose filesystem payload contents in errors.

- [ ] **Step 5: Run all HTTP tests**

Run: `cargo test -p consilium --test server_test api_`

Expected: PASS for token rejection, origin rejection, expiry, single-use preflight, per-repository locking, stale apply, and terminal result conflict.

- [ ] **Step 6: Commit**

```bash
git add core/src/server core/src/server.rs core/src/lib.rs core/tests/server_test.rs
git commit -m "feat: expose prepared safety resources"
```

### Task 3: Prepared WebSocket execution and result-ready lifecycle

**Files:**
- Modify: `core/src/server.rs` or `core/src/server/mod.rs`
- Modify: `core/src/orchestrator/council.rs`
- Modify: `core/src/orchestrator/review.rs`
- Modify: `core/tests/server_test.rs`
- Modify: `ui/src/protocol/ServerFrame.ts`
- Modify: `ui/src/protocol/SessionRequest.ts`

**Interfaces:**
- Consumes: `SafetyService::take_preflight`, `prepare_write_run`, existing `run_conduct`, and result finalization.
- Produces: `RunExecutor`, WebSocket `Start` handling for all three actions, safe legacy `Conduct` handling, `ResultReady`, and `SessionComplete` frames.

- [ ] **Step 1: Replace the existing in-place server assertion with a failing safe-source assertion**

```rust
#[tokio::test]
async fn prepared_start_leaves_original_untouched_and_returns_result_id() {
    let fixture = WsFixture::new_scripted().await;
    let prepared = fixture.prepare_build().await;
    let mut socket = fixture.connect().await;
    socket.send_json(serde_json::json!({
        "kind": "start",
        "preflight_id": prepared.id,
        "acceptance": { "command_digest": prepared.report.command_digest, "in_place_acknowledged": false }
    })).await;
    let frames = socket.collect_until_terminal().await;
    assert!(frames.iter().any(|f| f["type"] == "result_ready"));
    assert_eq!(std::fs::read_to_string(fixture.repo.join("base.txt")).unwrap(), "base\n");
}
```

- [ ] **Step 2: Run and confirm Start is rejected**

Run: `cargo test -p consilium --test server_test prepared_start_leaves_original_untouched_and_returns_result_id`

Expected: FAIL because `Start` is not handled and/or the source is still edited in place.

- [ ] **Step 3: Route Start through the prepared record and safety execution directory**

Define one injectable executor so server tests never call provider CLIs:

```rust
pub trait RunExecutor: Send + Sync + 'static {
    fn prepare(&self, report: SafetyPreflightReport, mode: ExecutionMode, acceptance: PreflightAcceptance) -> anyhow::Result<PreparedWriteRun>;
    fn conduct<'a>(&'a self, request: PreflightHttpRequest, cwd: &'a Path, sink: SessionEventSink, control: RunControl) -> futures::future::BoxFuture<'a, anyhow::Result<ConductOutcome>>;
    fn council<'a>(&'a self, request: PreflightHttpRequest, cwd: &'a Path, sink: SessionEventSink, control: RunControl) -> futures::future::BoxFuture<'a, anyhow::Result<CouncilOutcome>>;
    fn review<'a>(&'a self, request: PreflightHttpRequest, cwd: &'a Path, sink: SessionEventSink, control: RunControl) -> futures::future::BoxFuture<'a, anyhow::Result<ReviewResult>>;
    fn finalize(&self, prepared: PreparedWriteRun, outcome: ConductOutcome) -> anyhow::Result<ResultBundle>;
}
```

Expose thin event-sink parameters from existing `run_council` and `run_review` rather than reimplementing their model logic in the server.

```rust
SessionRequest::Start { preflight_id, acceptance } => {
    let record = state.safety.take_preflight(&preflight_id)?;
    validate_acceptance(&record.prepared.report, &acceptance)?;
    match record.request.action {
        ProductAction::Build => {
            let prepared = state.runner.prepare(record.prepared.report, record.request.execution_mode, acceptance)?;
            let outcome = state.runner.conduct(record.request, &prepared.execution_cwd, sink.clone(), control).await?;
            let bundle = state.runner.finalize(prepared, outcome).await?;
            state.safety.insert_result(bundle.clone())?;
            sink.server(ServerFrame::ResultReady { result_id: bundle.id.clone() }).await?;
            sink.server(ServerFrame::SessionComplete { result_id: Some(bundle.id) }).await?;
        }
        ProductAction::AskCouncil => {
            state.runner.council(record.request, &record.prepared.report.repository.canonical_path, sink.clone(), control).await?;
            sink.server(ServerFrame::SessionComplete { result_id: None }).await?;
        }
        ProductAction::ReviewChanges => {
            state.runner.review(record.request, &record.prepared.report.repository.canonical_path, sink.clone(), control).await?;
            sink.server(ServerFrame::SessionComplete { result_id: None }).await?;
        }
    }
}
```

Build alone acquires the write/repository lock and produces an Apply/Discard bundle. Ask Council and Review Changes run against the canonical source as read-only operations, preserve existing council/review output events, and never create a worktree. Full diff and binary files remain behind result HTTP endpoints. Release the repository lock on every terminal/error/cancel path.

- [ ] **Step 4: Preserve legacy Conduct safely**

For `SessionRequest::Conduct`, construct a local deterministic preflight and use safe-worktree mode only when no repository-config command requires a trust decision. Otherwise return `ServerFrame::PreflightInvalidated { reason: "This repository defines verification commands; refresh and approve the safety preview." }`. Never preserve the old direct in-place behavior.

- [ ] **Step 5: Run WebSocket lifecycle regressions**

Run: `cargo test -p consilium --test server_test ws_ && cargo test -p consilium --test server_test prepared_`

Expected: PASS for start, cancel, socket close, expiry, legacy safe mode, and repository lock release.

- [ ] **Step 6: Commit**

```bash
git add core/src/server.rs core/src/server core/src/orchestrator/council.rs core/src/orchestrator/review.rs core/tests/server_test.rs ui/src/protocol
git commit -m "feat: stream prepared runs to reviewable results"
```

### Task 4: Correct desktop workspace ownership and runtime bootstrap

**Files:**
- Modify: `desktop/src-tauri/src/commands.rs`
- Modify: `desktop/src-tauri/src/lib.rs`
- Modify: `ui/src/runtime.ts`
- Create: `ui/src/runtime.test.ts`

**Interfaces:**
- Consumes: existing Tauri `pick_workspace`, `get_server_state`, and embedded-server restart behavior.
- Produces: `WorkspaceRuntime`, `getWorkspace`, `chooseWorkspace`, `getSessionToken`, and resettable server-base caching.

- [ ] **Step 1: Write failing runtime adapter tests**

```ts
import { describe, expect, it, vi } from 'vitest'
import { createRuntime } from './runtime'

it('uses the native picker and refreshes the embedded server base', async () => {
  const invoke = vi.fn()
    .mockResolvedValueOnce({ baseUrl: 'http://127.0.0.1:7001', workspace: '/old' })
    .mockResolvedValueOnce('/new')
    .mockResolvedValueOnce({ baseUrl: 'http://127.0.0.1:7002', workspace: '/new' })
  const runtime = createRuntime({ isTauri: true, invoke, fetch: vi.fn() as never })
  await runtime.getWorkspace()
  expect(await runtime.chooseWorkspace()).toEqual({ path: '/new', baseUrl: 'http://127.0.0.1:7002' })
  expect(invoke).toHaveBeenCalledWith('pick_workspace')
})

it('explains that web workspace is fixed', async () => {
  const runtime = createRuntime({ isTauri: false, invoke: vi.fn(), fetch: vi.fn() as never })
  await expect(runtime.chooseWorkspace()).rejects.toThrow('Restart Consilium from the folder you want to use')
})
```

- [ ] **Step 2: Run and confirm current raw-dialog behavior is not represented**

Run: `npm --prefix ui test -- runtime.test.ts`

Expected: FAIL because `createRuntime`/`chooseWorkspace` do not exist.

- [ ] **Step 3: Implement one runtime contract**

```ts
export interface WorkspaceInfo { path: string; baseUrl: string }
export interface WorkspaceRuntime {
  getWorkspace(): Promise<WorkspaceInfo>
  chooseWorkspace(): Promise<WorkspaceInfo>
  getSessionToken(): Promise<string>
}

export function createRuntime(deps: RuntimeDeps): WorkspaceRuntime {
  let cached: WorkspaceInfo | undefined
  const load = async () => cached ??= deps.isTauri
    ? await waitForServerState(deps.invoke)
    : { path: '.', baseUrl: window.location.origin }
  return {
    getWorkspace: load,
    async chooseWorkspace() {
      if (!deps.isTauri) throw new Error('Restart Consilium from the folder you want to use')
      await deps.invoke('pick_workspace')
      cached = undefined
      return load()
    },
    async getSessionToken() {
      const { baseUrl } = await load()
      const response = await deps.fetch(`${baseUrl}/api/session-token`)
      if (!response.ok) throw new Error('Could not establish a local Consilium session')
      return (await response.json()).token
    },
  }
}
```

The Tauri command returns only after the old server stops and the new state reports the selected canonical workspace. Disable workspace changes while a run or Apply/Discard transition is active.

- [ ] **Step 4: Run Rust and TypeScript runtime tests**

Run: `cargo test -p consilium-desktop && npm --prefix ui test -- runtime.test.ts && npm --prefix ui run typecheck`

Expected: PASS; no infinite poll after workspace restart.

- [ ] **Step 5: Commit**

```bash
git add desktop/src-tauri/src/commands.rs desktop/src-tauri/src/lib.rs ui/src/runtime.ts ui/src/runtime.test.ts
git commit -m "fix: make desktop workspace selection authoritative"
```

### Task 5: Run-flow reducer, authenticated API client, and controller hook

**Files:**
- Create: `ui/src/runFlow/types.ts`
- Create: `ui/src/runFlow/reducer.ts`
- Create: `ui/src/runFlow/reducer.test.ts`
- Create: `ui/src/runFlow/api.ts`
- Create: `ui/src/runFlow/api.test.ts`
- Create: `ui/src/runFlow/useRunFlow.ts`
- Modify: `ui/src/useSession.ts`

**Interfaces:**
- Consumes: generated protocol DTOs, `WorkspaceRuntime`, and existing live session reducer/events.
- Produces: `RunFlowState`, `RunFlowAction`, `runFlowReducer`, `SafetyApi`, and `useRunFlow`.

- [ ] **Step 1: Write failing legal-transition tests**

```ts
it('moves setup through preflight, run, and review without skipping confirmation', () => {
  let state = initialRunFlowState
  state = runFlowReducer(state, { type: 'workspace_ready', workspace: fixtureWorkspace })
  state = runFlowReducer(state, { type: 'preflight_requested' })
  expect(state.phase).toBe('preflighting')
  state = runFlowReducer(state, { type: 'preflight_ready', prepared: fixturePreflight })
  expect(state.phase).toBe('confirming')
  state = runFlowReducer(state, { type: 'run_started' })
  expect(state.phase).toBe('running')
  state = runFlowReducer(state, { type: 'result_ready', result: fixtureResult })
  expect(state.phase).toBe('review')
})

it('keeps a stale result reviewable after apply conflict', () => {
  const state = runFlowReducer(reviewState, { type: 'transition_failed', operation: 'apply', error: 'Source changed' })
  expect(state.phase).toBe('review')
  expect(state.result?.state).toBe('ready')
  expect(state.error).toContain('Source changed')
})
```

- [ ] **Step 2: Run and confirm run-flow files are absent**

Run: `npm --prefix ui test -- runFlow/reducer.test.ts`

Expected: FAIL with module-not-found.

- [ ] **Step 3: Define the state machine with exhaustive transitions**

```ts
export type RunPhase = 'setup' | 'preflighting' | 'confirming' | 'running' | 'review' | 'applying' | 'discarding' | 'complete' | 'error'

export interface RunFlowState {
  phase: RunPhase
  workspace?: WorkspaceInfo
  draft: { action: ProductAction; task: string; context: string; mode: ExecutionMode }
  prepared?: PreparedPreflight
  result?: ResultSummary
  error?: string
}

export function runFlowReducer(state: RunFlowState, action: RunFlowAction): RunFlowState {
  switch (action.type) {
    case 'preflight_requested': return { ...state, phase: 'preflighting', error: undefined }
    case 'preflight_ready': return { ...state, phase: 'confirming', prepared: action.prepared }
    case 'run_started': return { ...state, phase: 'running', error: undefined }
    case 'result_ready': return { ...state, phase: 'review', result: action.result }
    case 'apply_started': return { ...state, phase: 'applying' }
    case 'discard_started': return { ...state, phase: 'discarding' }
    case 'transition_failed': return { ...state, phase: 'review', error: action.error }
    default: return reduceNonTerminal(state, action)
  }
}
```

- [ ] **Step 4: Implement token-authenticated fetch and Start WebSocket**

```ts
async function mutate<T>(path: string, body?: unknown): Promise<T> {
  const token = await runtime.getSessionToken()
  const response = await fetch(`${baseUrl}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-consilium-token': token },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const value = await response.json()
  if (!response.ok) throw new ApiError(response.status, value.error)
  return value as T
}

async function readProtected<T>(path: string): Promise<T> {
  const token = await runtime.getSessionToken()
  const response = await fetch(`${baseUrl}${path}`, { headers: { 'x-consilium-token': token } })
  const value = await response.json()
  if (!response.ok) throw new ApiError(response.status, value.error)
  return value as T
}
```

`useRunFlow` calls preflight, opens the socket with `Start`, forwards live agent events to the existing session reducer, fetches result summary on `ResultReady`, and exposes `apply`, `discard`, `cancel`, `pause`, `resume`, and `interject`. `SessionComplete { result_id: null }` sends Ask Council and Review Changes to a read-only completion panel; only Build can enter `review`, `applying`, or `discarding` phases.

- [ ] **Step 5: Run reducer/API tests and typecheck**

Run: `npm --prefix ui test -- runFlow && npm --prefix ui run typecheck`

Expected: PASS; all run phases and HTTP status mappings are exhaustive.

- [ ] **Step 6: Commit**

```bash
git add ui/src/runFlow ui/src/useSession.ts
git commit -m "feat: model the trust-first run lifecycle"
```

### Task 6: Setup and preflight confirmation surfaces

**Files:**
- Modify: `ui/package.json`
- Modify: `ui/package-lock.json`
- Modify: `ui/vite.config.ts`
- Create: `ui/src/components/run/ActionPicker.tsx`
- Create: `ui/src/components/run/ProjectPicker.tsx`
- Create: `ui/src/components/run/ProviderReadiness.tsx`
- Create: `ui/src/components/run/RunSetup.tsx`
- Create: `ui/src/components/run/PreflightReview.tsx`
- Create: `ui/src/components/run/RunSetup.test.tsx`
- Create: `ui/src/components/run/PreflightReview.test.tsx`
- Modify: `ui/src/components/StartRunForm.tsx`
- Modify: `ui/src/styles.css`

**Interfaces:**
- Consumes: `RunFlowState`, `useRunFlow`, and provider `DoctorReport`.
- Produces: accessible setup and confirmation UI with explicit quota probe and command trust.

- [ ] **Step 1: Add component-test dependencies and environment**

Run: `npm --prefix ui install --save-dev jsdom @testing-library/react @testing-library/user-event @testing-library/jest-dom`

Expected: `package.json` and lockfile change only; no production dependency added.

- [ ] **Step 2: Write failing user-facing tests**

```tsx
it('explains the three actions and does not probe providers automatically', async () => {
  const user = userEvent.setup()
  const onProbe = vi.fn()
  render(<RunSetup state={fixtureSetup} onProbeProviders={onProbe} onSubmit={vi.fn()} onChooseWorkspace={vi.fn()} />)
  expect(screen.getByRole('button', { name: 'Build' })).toBeVisible()
  expect(screen.getByText('Works in an isolated Git worktree until you apply the result.')).toBeVisible()
  expect(screen.getByRole('button', { name: 'Ask Council' })).toBeVisible()
  expect(screen.getByRole('button', { name: 'Review Changes' })).toBeVisible()
  expect(screen.getByText('Claude, Codex, Gemini, and Grok can build and cross-review a task while your original Git worktree stays untouched until you approve the diff.')).toBeVisible()
  expect(screen.getByRole('button', { name: 'Watch a safe demo' })).toBeVisible()
  expect(onProbe).not.toHaveBeenCalled()
  await user.click(screen.getByRole('button', { name: 'Check provider readiness' }))
  expect(onProbe).toHaveBeenCalledOnce()
})

it('shows exact repository commands and requires trust before build', async () => {
  render(<PreflightReview prepared={repositoryCommandPreflight} onConfirm={vi.fn()} onBack={vi.fn()} />)
  expect(screen.getByText('/workspace/consilium')).toBeVisible()
  expect(screen.getByText('cargo test --workspace')).toBeVisible()
  expect(screen.getByText('From repository config')).toBeVisible()
  expect(screen.getByRole('button', { name: 'Start safely' })).toBeDisabled()
})

it('does not claim worktree isolation for a non-git folder', () => {
  render(<PreflightReview prepared={nonGitPreflight} onConfirm={vi.fn()} onBack={vi.fn()} />)
  expect(screen.getByText('Safe worktree is unavailable because this folder is not a Git repository.')).toBeVisible()
  expect(screen.getByText('Initialize Git, choose a read-only action, or explicitly run in place.')).toBeVisible()
  expect(screen.queryByRole('button', { name: 'Start safely' })).not.toBeInTheDocument()
})

it('explains why apply will stay disabled for an initially dirty checkout', () => {
  render(<PreflightReview prepared={dirtyPreflight} onConfirm={vi.fn()} onBack={vi.fn()} />)
  expect(screen.getByText('The run starts from the reported HEAD. Clean this checkout before Apply; the manual patch will remain available.')).toBeVisible()
})
```

- [ ] **Step 3: Run and confirm components are missing**

Run: `npm --prefix ui test -- components/run`

Expected: FAIL with module-not-found.

- [ ] **Step 4: Build the setup hierarchy and plain-language copy**

The visible order is: one-sentence value proposition; project path; three action cards; task/context; Safe worktree/In place selector only for Build; “Preview safety” primary button; collapsed “Advanced details”; explicit “Check provider readiness” secondary button. `StartRunForm` becomes a compatibility wrapper around `RunSetup` and contains no raw Tauri dialog call.

Use exact copy:

```text
Claude, Codex, Gemini, and Grok can build and cross-review a task while your original Git worktree stays untouched until you approve the diff.
Build — Let several coding agents plan, implement, and review a change. Works in an isolated Git worktree until you apply the result.
Ask Council — Get multiple independent answers and a synthesis. Read-only.
Review Changes — Ask the council to review the current Git diff. Read-only.
Safe worktree — Your current checkout is not edited until you review and apply the result.
In place — Agents edit this folder directly. Use only when you understand the risk.
Single-provider mode — Consilium can continue with one ready provider, but cross-provider review needs at least one additional ready provider.
```

Each missing provider row shows its exact `hint` from `DoctorReport` (install or login command) beside the status. A readiness row with one ready provider shows the single-provider explanation above; zero ready providers disables the real start button but leaves `Watch a safe demo` enabled.

- [ ] **Step 5: Build the preflight hierarchy**

Show path and Git state first, then mode, provider roles, verification commands grouped by source, timeouts/budget, and warnings. Repository-config commands require a checkbox labelled `Trust these exact commands for this project`; in-place requires a separate checkbox labelled `I understand that agents will edit this folder directly`.

- [ ] **Step 6: Run component tests, accessibility assertions, and typecheck**

Run: `npm --prefix ui test -- components/run && npm --prefix ui run typecheck`

Expected: PASS; primary actions are keyboard reachable, warnings are associated with controls, and no automatic doctor request occurs.

- [ ] **Step 7: Commit**

```bash
git add ui/package.json ui/package-lock.json ui/vite.config.ts ui/src/components/run ui/src/components/StartRunForm.tsx ui/src/styles.css
git commit -m "feat: add trust-first setup and preflight review"
```

### Task 7: Result review, demo scenario, and single shared Table entry point

**Files:**
- Create: `ui/src/components/run/ResultReview.tsx`
- Create: `ui/src/components/run/ResultReview.test.tsx`
- Modify: `ui/src/components/ResultPanel.tsx`
- Create: `ui/src/demoScenario.ts`
- Modify: `ui/src/demoSession.ts`
- Modify: `ui/src/App.tsx`
- Modify: `ui/src/components/TableView.tsx`
- Modify: `ui/src/reducer.ts`
- Modify: `ui/src/styles.css`
- Create: `ui/src/App.test.tsx`

**Interfaces:**
- Consumes: result summary/diff API, `useRunFlow`, existing live reducer, and Table visuals.
- Produces: `ResultReview`, complete quota-free `demoScenario`, and one shared setup surface across Run/Table views.

- [ ] **Step 1: Write failing result and demo tests**

```tsx
it('requires diff review before apply and preserves stale results', async () => {
  const user = userEvent.setup()
  const onApply = vi.fn()
  render(<ResultReview result={readyResult} diff={fixtureDiff} onApply={onApply} onDiscard={vi.fn()} />)
  expect(screen.getByText('Original checkout unchanged')).toBeVisible()
  expect(screen.getByText('src/lib.rs')).toBeVisible()
  await user.click(screen.getByRole('checkbox', { name: 'I reviewed these changes' }))
  await user.click(screen.getByRole('button', { name: 'Apply to my checkout' }))
  expect(onApply).toHaveBeenCalledOnce()
})

it('runs the complete demo without fetch or websocket', async () => {
  const network = vi.spyOn(globalThis, 'fetch')
  render(<App demoScenario={demoScenario} />)
  await userEvent.click(screen.getByRole('button', { name: 'Watch a safe demo' }))
  expect(await screen.findByText('Original checkout unchanged')).toBeVisible()
  expect(network).not.toHaveBeenCalled()
})
```

- [ ] **Step 2: Run and confirm the review/demo flow is absent**

Run: `npm --prefix ui test -- ResultReview App.test.tsx`

Expected: FAIL with missing component or old terminal panel behavior.

- [ ] **Step 3: Implement result review with safe terminal actions**

```tsx
export function ResultReview({ result, diff, reviewed, onReviewed, onApply, onDiscard }: Props) {
  const ready = result.state === 'ready'
  return <section aria-labelledby="result-title">
    <h2 id="result-title">Review changes</h2>
    <p>{result.warning ?? 'Original checkout unchanged'}</p>
    <VerificationSummary outcomes={result.verification} />
    <DiffViewer diff={diff} files={result.changed_files} />
    {ready && <label><input type="checkbox" checked={reviewed} onChange={e => onReviewed(e.target.checked)} /> I reviewed these changes</label>}
    <button disabled={!ready || !reviewed} onClick={onApply}>Apply to my checkout</button>
    <button disabled={!ready} onClick={onDiscard}>Discard worktree</button>
  </section>
}
```

Binary files display path, byte size, and digest rather than attempting a text diff. A 409 Apply error remains on this screen with instructions to clean/restore the source checkout; Discard never deletes the transcript/bundle.

- [ ] **Step 4: Replace frame-only demo with a full scenario object**

```ts
export const demoScenario: DemoScenario = {
  workspace: { path: '/demo/consilium', baseUrl: 'demo://' },
  preflight: demoPreparedPreflight,
  frames: demoAgentFrames,
  result: demoReadyResult,
  diff: 'diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1,2 @@\n export const ready = true\n+export const reviewed = true\n',
}
```

Demo transitions through confirming/running/review and permits only local simulated Discard; label Apply as simulated if offered.

- [ ] **Step 5: Make Run and Table share one flow**

`App.tsx` owns one `useRunFlow`. Remove the second `StartRunForm` from `TableView`; when idle, both navigation tabs render the same `RunSetup`, and while running Table displays the medical visualization of the same live events. Provider attribution comes from task/session IDs, never “latest session started”.

- [ ] **Step 6: Run UI tests and production build**

Run: `npm --prefix ui test && npm --prefix ui run typecheck && npm --prefix ui run build`

Expected: PASS; demo performs no network calls and both Run/Table paths reach the same result ID.

- [ ] **Step 7: Commit**

```bash
git add ui/src/components/run/ResultReview.tsx ui/src/components/run/ResultReview.test.tsx ui/src/components/ResultPanel.tsx ui/src/demoScenario.ts ui/src/demoSession.ts ui/src/App.tsx ui/src/App.test.tsx ui/src/components/TableView.tsx ui/src/reducer.ts ui/src/styles.css
git commit -m "feat: review and apply isolated results"
```

### Task 8: End-to-end trust-first acceptance gate

**Files:**
- Modify only if a gate exposes a defect: files introduced in Tasks 1–7.

**Interfaces:**
- Consumes: completed safety core and trust-first UI.
- Produces: locally verified browser → API → worktree → result → Apply/Discard flows.

- [ ] **Step 1: Run protocol/server/Desktop tests**

Run: `cargo test -p consilium --test server_test && cargo test -p consilium-desktop`

Expected: PASS; no provider quota and no network.

- [ ] **Step 2: Run the full frontend gate**

Run: `npm --prefix ui ci && npm --prefix ui run typecheck && npm --prefix ui test && npm --prefix ui run build`

Expected: PASS with a production bundle.

- [ ] **Step 3: Run workspace Rust gates**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

Expected: PASS with no warnings.

- [ ] **Step 4: Exercise both terminal paths locally**

Start the local server against a disposable committed fixture. Through the browser: Build → preview → safe start → review → Apply; repeat and choose Discard. Confirm with `git status --porcelain=v1` that the source changes only after Apply, and that Discard removes the worktree while preserving the result bundle.

- [ ] **Step 5: Verify demo isolation**

Run the UI with backend unavailable, click `Watch a safe demo`, and inspect the browser network log.

Expected: the flow reaches Result Review with zero HTTP/WebSocket requests and no provider process.

- [ ] **Step 6: Commit any gate-only corrections**

```bash
git add core desktop ui
git commit -m "test: verify trust-first onboarding end to end"
```

Skip this commit when the gate required no changes.
