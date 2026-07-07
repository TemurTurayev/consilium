# Consilium Desktop — Tauri app + installers (design)

**Date:** 2026-07-07
**Status:** APPROVED (autonomous session; steer by exception)
**Scope:** macOS (Apple Silicon + Intel) and Linux (AppImage + .deb). Native Windows is
**deferred** — the core drives agent CLIs through `sh -c`, `$HOME`, and Unix process
semantics; a Windows build would install but could not run a single agent. Windows users
keep the documented WSL path until the core grows native Windows support (tracked as
follow-on work, not this spec).

## Why a desktop app

The web UI already exists (Vite + React over `consilium serve`), but using it requires a
terminal: start the server, open a browser, type a cwd by hand. The desktop app removes
the terminal from the loop: double-click, pick a project folder in a native dialog, run.
The README roadmap already names "Tauri desktop app" for v1.1+ — this implements it.

## Approaches considered

1. **Tauri 2, axum server embedded in-process** (chosen). The Tauri backend is Rust, so it
   links the `consilium` core crate directly, runs the existing axum server on
   `127.0.0.1:0`, and points the webview at the existing React UI. Near-total reuse of
   both the server and the UI; one binary; installers come from `tauri-bundler` for free.
2. Tauri with IPC commands instead of WebSocket — replaces a working, tested WS protocol
   with a second transport; the UI's `useSession` would fork into two code paths. Rejected.
3. Electron — a second runtime (Node), ~150 MB installers, and none of the core reuse
   (server would run as a sidecar subprocess). Rejected.

Sidecar note: we deliberately do NOT ship the CLI binary inside the app as a sidecar.
The server runs in-process (same crate), which removes process-management failure modes
and keeps the app a single artifact.

## Architecture

```
desktop/src-tauri (new crate: consilium-desktop)
  ├─ links `consilium` core (path dep)
  ├─ owns: workspace selection, server lifecycle, native dialogs
  ├─ starts core::server on 127.0.0.1:0  ← launch_root = chosen workspace
  └─ webview loads ui/dist (frontendDist), talks to the server over WS/HTTP
ui/ (existing React app — stays Tauri-free)
  └─ new runtime resolver: window.__TAURI__ present → invoke commands; else env/same-origin
core/ (existing)
  └─ additions: bindable serve API, run cancellation, /api/doctor, /api/config, /api/version
```

### Server lifecycle (desktop backend)

- On launch, read `~/.consilium/desktop.json` (`{ "workspace": "/abs/path" }`). If a saved
  workspace exists and is a directory, start the server immediately; otherwise the UI shows
  the workspace-picker empty state.
- `pick_workspace` Tauri command: native folder dialog → persist choice → (re)start the
  server with `launch_root =` the chosen folder. Restart = abort the old server task, bind
  a fresh `127.0.0.1:0` listener. Config is loaded per workspace:
  `<workspace>/consilium.config.json` if present, else `~/.consilium/consilium.config.json`,
  else `Config::default()` (matches `Config::load`'s missing-file behavior).
- `get_server_state` Tauri command → `{ serverUrl: string | null, workspace: string | null }`.
  The UI polls this once at startup and after `pick_workspace`.
- The UI stays free of `@tauri-apps/api` npm deps: `app.withGlobalTauri = true`, and the UI
  feature-detects `window.__TAURI__` (invoke, dialog). In a plain browser the same code
  falls back to `VITE_WS_URL`/`VITE_API_URL`/same-origin — web mode keeps working unchanged.

### Core changes

1. **Bindable serve.** `serve(addr, …)` binds internally and never reports the port. Add:
   `pub async fn serve_on(listener: tokio::net::TcpListener, config, quota, timeout,
   launch_root: PathBuf) -> Result<()>` — the existing `serve` becomes a thin wrapper
   (bind + current-dir launch_root + `serve_on`). Desktop binds `:0` itself, reads
   `local_addr()`, passes the listener in. `with_launch_root` stops being test-only.
2. **Run cancellation** (also closes a known gap: closing the browser/tab kept runs
   burning quota). In `handle_session`, spawn the run as an abortable task and keep
   polling the client socket. A client text frame `{"kind":"cancel"}` **or** socket close
   aborts the run task; dropped futures SIGKILL children via the existing
   `kill_on_drop(true)` discipline. New terminal frame `ServerFrame::RunCancelled`
   (ts-rs export + regenerated bindings). One run at a time per server: a second
   `/ws/session` while a run is active gets `ServerFrame::Error` ("run already active") —
   closes the concurrent-runs-in-one-cwd gap from the last audit.
3. **Desktop endpoints** (all ts-rs typed, all tested):
   - `GET /api/doctor` → `DoctorReport { providers: Vec<ProviderStatus> }` wrapping
     `auth::auth_report()`; `ProviderStatus { provider, state: ready|needs_login|
     cli_missing|down, detail, hint }` where `hint` is the exact next command
     (`claude setup-token` / `codex login` / `agy login`). Probes run live; the endpoint
     is fetch-on-demand (Providers view refresh button), never polled.
   - `GET /api/config` → `ConfigSummary { conductor, workers, reviewer, chairman,
     supervisor, cross_family_review, budget_secs, config_path }` (read-only; editing
     stays in `consilium init` for v1).
   - `GET /api/version` → `{ version: env!("CARGO_PKG_VERSION") }`.
4. **Origin allowlist**: add `tauri.localhost` (Linux/Windows webview origin is
   `http://tauri.localhost`; macOS `tauri://localhost` already passes as host
   `localhost`). Tests for both, plus the existing evil-suffix cases.
5. **Protocol guard**: add the missing test that `AgentEvent` and `ServerFrame` tag
   namespaces stay disjoint (the invariant `InboundFrame` relies on).

### UI changes

- **Runtime base URL** (`ui/src/runtime.ts`): order — Tauri `get_server_state` →
  `VITE_WS_URL`/`VITE_API_URL` → same-origin defaults. `wsUrl.ts` and `useQuota` consume it.
- **App shell**: left sidebar — **Run**, **Usage**, **Providers**, **Settings** — replacing
  the two header tabs. Content column keeps the current max-width and visual language
  (hand-rolled CSS, provider tokens); add a light theme via `prefers-color-scheme` with the
  existing dark palette as default.
- **Run view**: existing form + session stream + a **Stop** button while a run is active
  (sends the cancel frame; reducer handles `run_cancelled` as a terminal state). The cwd
  field gains a **Browse…** button when `window.__TAURI__` exists (native folder dialog,
  confined paths only — the server still enforces `cwd_within_root`).
- **Providers view**: doctor report cards per provider (state, detail, copyable auth hint,
  Refresh). Doubles as onboarding: when zero providers are ready, Run view shows a banner
  pointing here.
- **Settings view**: read-only `ConfigSummary` + workspace path + "Change workspace…"
  (Tauri only) + app/server version.
- **Bug fix in scope**: `ResultPanel` prioritizes a stale mid-run `error` over the
  terminal frame (found in the last audit) — `run_complete`/`run_cancelled` clear it.
- Reducer stays a pure fold; every new frame/action gets vitest coverage; regenerated
  ts-rs bindings stay committed.

### Packaging & CI

- `desktop/src-tauri/tauri.conf.json`: `frontendDist: ../../ui/dist`,
  `beforeBuildCommand: npm --prefix ../../ui run build`, `withGlobalTauri: true`,
  plugins: `dialog` (folder picker), `notification` (run-finished toast when unfocused).
  Icons generated from a new `desktop/icon.svg` (three-circle consilium mark) via
  `cargo tauri icon`.
- Local deliverable now: unsigned aarch64 `.dmg` + `.app`, verified by launching.
  Unsigned = macOS Gatekeeper requires right-click → Open on first launch; README notes
  this. Code signing/notarization deferred until there is an Apple Developer ID.
- CI (`release.yml`, on `v*` tags, gated on the test job like the CLI build):
  - `macos-14`: `cargo tauri build` for `aarch64-apple-darwin` and
    `--target x86_64-apple-darwin` → two `.dmg`.
  - `ubuntu-22.04`: `.AppImage` + `.deb`.
  - SHA256 sidecar files for every artifact (same pattern as CLI tarballs).
- Version: `consilium-desktop` version tracks the core crate version.

## Error handling

- Server task panics/exits → desktop backend keeps the app alive, surfaces a
  "server stopped — restart?" state via `get_server_state` (`serverUrl: null`,
  `error: string`), UI offers restart (re-invokes workspace start).
- Workspace deleted while saved → treated as no-workspace (picker state), stale entry
  cleared from `desktop.json`.
- Cancel during verify/build: the verify child is killed by the same abort (kill_on_drop);
  quota rows already written stay (accurate accounting of spent tokens).

## Testing

- Core: unit + integration tests for `serve_on` port-0 wiring, cancellation
  (client-close kills run; cancel frame yields `run_cancelled`; concurrent session
  rejected), doctor/config/version endpoints (scripted adapters), origin additions,
  tag-disjointness guard. `cargo fmt` + clippy `-D warnings` + full suite green — and CI
  now enforces this on every push.
- UI: vitest for runtime resolver (env fallback), reducer `run_cancelled`, ResultPanel
  ordering fix, providers view helpers. `tsc -b` clean.
- Desktop: `cargo build` in CI on both platforms; manual E2E on macOS now (launch app,
  pick workspace, demo run, real run if a provider is authed, screenshot as proof).

## Out of scope (named, deliberate)

- Native Windows support (core-level work; separate spec).
- Config *editing* UI (v1 is read-only; `consilium init` remains the editor).
- Council/live-deliberation view (stays the next web-UI slice; the desktop shell will
  inherit it when it lands).
- Auto-update (needs signing first).
- Multi-workspace / multiple concurrent runs per server.
