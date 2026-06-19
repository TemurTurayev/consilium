/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** WebSocket endpoint of the consilium backend. */
  readonly VITE_WS_URL?: string
  /** Base URL for REST calls (e.g. `/api/quota`). Empty/unset = same-origin. */
  readonly VITE_API_URL?: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}
