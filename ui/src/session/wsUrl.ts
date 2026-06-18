// Matches `consilium serve --addr 127.0.0.1:7878` out of the box.
const DEFAULT_WS_URL = 'ws://localhost:7878/ws/session'

/** The backend WebSocket endpoint, overridable via `VITE_WS_URL`. */
export function resolveWsUrl(): string {
  return import.meta.env.VITE_WS_URL ?? DEFAULT_WS_URL
}
