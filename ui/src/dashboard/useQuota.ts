import { useCallback, useEffect, useRef, useState } from 'react'
import type { QuotaSnapshot } from '../protocol'

const POLL_MS = 10_000

/** Resolve a REST path against the backend. Empty base = same-origin: dev goes
 * through the Vite `/api` proxy, prod serves the UI from the server. Override
 * the base with `VITE_API_URL` (e.g. when the UI is hosted separately). */
function apiUrl(path: string): string {
  const base = import.meta.env.VITE_API_URL?.trim() ?? ''
  return `${base}${path}`
}

export interface QuotaState {
  snapshot: QuotaSnapshot | null
  error: string | null
  loading: boolean
}

const initial: QuotaState = { snapshot: null, error: null, loading: false }

/** Owns the `/api/quota` fetch + polling. Polls only while `active` (the Usage
 * tab is open), and refetches on demand. The only impure layer of the dashboard. */
export function useQuota(active: boolean): { state: QuotaState; refresh: () => void } {
  const [state, setState] = useState<QuotaState>(initial)
  const abortRef = useRef<AbortController | null>(null)

  const refresh = useCallback(async () => {
    abortRef.current?.abort()
    const ctrl = new AbortController()
    abortRef.current = ctrl
    setState((s) => ({ ...s, loading: true }))
    try {
      const res = await fetch(apiUrl('/api/quota'), { signal: ctrl.signal })
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
      const snapshot = (await res.json()) as QuotaSnapshot
      setState({ snapshot, error: null, loading: false })
    } catch (err) {
      if (err instanceof DOMException && err.name === 'AbortError') return
      setState((s) => ({ ...s, error: (err as Error).message, loading: false }))
    }
  }, [])

  useEffect(() => {
    if (!active) return
    void refresh()
    const id = setInterval(() => void refresh(), POLL_MS)
    return () => {
      clearInterval(id)
      abortRef.current?.abort()
    }
  }, [active, refresh])

  return { state, refresh }
}
