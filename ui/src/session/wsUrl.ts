import { resolveServerBase } from '../runtime'

/** The backend WebSocket endpoint. Resolved via `resolveServerBase` — Tauri
 * IPC in the desktop shell, `VITE_WS_URL`/`VITE_API_URL` in a plain web
 * build, same-origin defaults otherwise. */
export async function resolveWsUrl(): Promise<string> {
  const { ws } = await resolveServerBase()
  return ws
}
