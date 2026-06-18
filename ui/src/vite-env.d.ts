/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** WebSocket endpoint of the consilium backend. */
  readonly VITE_WS_URL?: string
}

interface ImportMeta {
  readonly env: ImportMetaEnv
}
