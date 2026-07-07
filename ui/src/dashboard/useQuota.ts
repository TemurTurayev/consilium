import { useCallback, useEffect, useRef, useState } from 'react'
import type { QuotaSnapshot } from '../protocol'
import { apiUrl } from '../runtime'

const POLL_MS = 10_000

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
      const url = await apiUrl('/api/quota')
      const res = await fetch(url, { signal: ctrl.signal })
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
