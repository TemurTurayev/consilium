// Resolves where the backend lives. Three environments share this UI build:
//  (a) inside the Tauri desktop shell — ask the Rust side, which starts the
//      server only after the user picks a workspace, so `serverUrl` may be
//      null for a moment;
//  (b) a plain web build with `VITE_API_URL` / `VITE_WS_URL` overrides;
//  (c) a plain web build with no overrides — same-origin defaults.

// Matches `consilium serve --addr 127.0.0.1:7878` out of the box.
const DEFAULT_WS = 'ws://localhost:7878/ws/session'
const POLL_MS = 500

export interface ServerBase {
  http: string
  ws: string
}

interface TauriServerState {
  serverUrl: string | null
  workspace: string | null
  error: string | null
}

interface TauriGlobal {
  core: { invoke: (cmd: string) => Promise<TauriServerState> }
}

function tauriGlobal(): TauriGlobal | null {
  const w = window as unknown as { __TAURI__?: TauriGlobal }
  return w.__TAURI__ ?? null
}

/** Derive a `ws://…/ws/session` endpoint from an `http(s)://` server URL. */
export function deriveWsFromHttp(http: string): string {
  const wsProto = http.startsWith('https://') ? 'wss://' : 'ws://'
  const rest = http.replace(/^https?:\/\//, '').replace(/\/+$/, '')
  return `${wsProto}${rest}/ws/session`
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

async function resolveFromTauri(tauri: TauriGlobal): Promise<ServerBase> {
  for (;;) {
    const state = await tauri.core.invoke('get_server_state')
    if (state.serverUrl) {
      const http = state.serverUrl.replace(/\/+$/, '')
      return { http, ws: deriveWsFromHttp(http) }
    }
    await sleep(POLL_MS)
  }
}

/** The subset of `ImportMetaEnv` this module reads — narrowed so tests can
 * pass plain object literals instead of a full Vite env shape. */
export interface RuntimeEnv {
  readonly VITE_API_URL?: string
  readonly VITE_WS_URL?: string
}

/** Env-driven resolution for plain web builds (no Tauri). Exported separately
 * so tests can exercise it without touching `window.__TAURI__`. */
export function resolveFromEnv(env: RuntimeEnv): ServerBase {
  const httpOverride = env.VITE_API_URL?.trim()
  const wsOverride = env.VITE_WS_URL?.trim()
  return {
    http: httpOverride ? httpOverride : '',
    ws: wsOverride ? wsOverride : httpOverride ? deriveWsFromHttp(httpOverride) : DEFAULT_WS,
  }
}

let cached: Promise<ServerBase> | null = null

/** Resolve the backend's HTTP + WS base once per page load, cached thereafter.
 * `http` is `''` for same-origin REST calls (matches the historical `apiUrl`
 * behavior); `ws` is always a full `ws://`/`wss://` URL. */
export function resolveServerBase(): Promise<ServerBase> {
  if (!cached) {
    const tauri = tauriGlobal()
    cached = tauri ? resolveFromTauri(tauri) : Promise.resolve(resolveFromEnv(import.meta.env))
  }
  return cached
}

/** Test-only escape hatch to clear the module-level cache between cases. */
export function __resetServerBaseCacheForTests(): void {
  cached = null
}

/** Resolve a REST path against the backend. Empty base = same-origin: dev
 * goes through the Vite `/api` proxy, prod serves the UI from the server (or,
 * in the desktop shell, the resolved Tauri server URL). */
export async function apiUrl(path: string): Promise<string> {
  const { http } = await resolveServerBase()
  return `${http}${path}`
}
