import { describe, expect, it } from 'vitest'
import { deriveWsFromHttp, resolveFromEnv } from './runtime'

describe('deriveWsFromHttp', () => {
  it('derives ws:// from http://', () => {
    expect(deriveWsFromHttp('http://localhost:7878')).toBe('ws://localhost:7878/ws/session')
  })

  it('derives wss:// from https://', () => {
    expect(deriveWsFromHttp('https://example.com')).toBe('wss://example.com/ws/session')
  })

  it('strips a trailing slash before appending the path', () => {
    expect(deriveWsFromHttp('http://localhost:7878/')).toBe('ws://localhost:7878/ws/session')
  })
})

describe('resolveFromEnv', () => {
  it('falls back to same-origin http and the default ws url when unset', () => {
    const base = resolveFromEnv({})
    expect(base.http).toBe('')
    expect(base.ws).toBe('ws://localhost:7878/ws/session')
  })

  it('treats a whitespace-only override as unset (common .env mistake)', () => {
    const base = resolveFromEnv({ VITE_API_URL: '   ', VITE_WS_URL: '  ' })
    expect(base.http).toBe('')
    expect(base.ws).toBe('ws://localhost:7878/ws/session')
  })

  it('derives ws from VITE_API_URL when only the REST base is overridden', () => {
    const base = resolveFromEnv({ VITE_API_URL: 'http://example.com:9000' })
    expect(base.http).toBe('http://example.com:9000')
    expect(base.ws).toBe('ws://example.com:9000/ws/session')
  })

  it('honors an explicit VITE_WS_URL independent of VITE_API_URL', () => {
    const base = resolveFromEnv({
      VITE_API_URL: 'http://example.com:9000',
      VITE_WS_URL: 'wss://other-host/ws/custom',
    })
    expect(base.http).toBe('http://example.com:9000')
    expect(base.ws).toBe('wss://other-host/ws/custom')
  })
})

// `resolveServerBase`'s Tauri branch touches `window.__TAURI__`, which only
// exists in the desktop shell's webview. It's exercised manually there; the
// suite here (node env, per project convention — see reducer/parseFrame
// tests) covers the pure env-fallback resolution `resolveFromEnv` performs,
// which is the byte-compatible-with-today path this refactor must preserve.
