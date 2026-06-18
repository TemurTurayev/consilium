import { useCallback, useEffect, useReducer, useRef } from 'react'
import type { SessionRequest } from '../protocol'
import { resolveWsUrl } from './wsUrl'
import { parseFrame } from './parseFrame'
import { initialState, sessionReducer, type SessionState } from './reducer'
import { demoSession } from './demoSession'

export interface UseSession {
  state: SessionState
  start: (req: SessionRequest) => void
  startDemo: () => void
  reset: () => void
}

const DEMO_STEP_MS = 380

/** The only impure layer: owns the WebSocket (and the demo timers), dispatching
 * parsed frames + socket lifecycle into the pure reducer. */
export function useSession(): UseSession {
  const [state, dispatch] = useReducer(sessionReducer, initialState)
  const wsRef = useRef<WebSocket | null>(null)
  const timersRef = useRef<ReturnType<typeof setTimeout>[]>([])

  const teardown = useCallback(() => {
    const ws = wsRef.current
    if (ws) {
      // Detach handlers BEFORE close(): close() is async, so otherwise the old
      // socket's late close/error/message events would dispatch into fresh state
      // (corrupting a reset screen, or flipping a back-to-back run to closed).
      ws.onopen = ws.onmessage = ws.onerror = ws.onclose = null
      ws.close()
    }
    wsRef.current = null
    timersRef.current.forEach((t) => clearTimeout(t))
    timersRef.current = []
  }, [])

  // Close the socket / clear timers when the component unmounts.
  useEffect(() => teardown, [teardown])

  const start = useCallback(
    (req: SessionRequest) => {
      teardown()
      dispatch({ type: 'start' })

      const ws = new WebSocket(resolveWsUrl())
      wsRef.current = ws

      ws.onopen = () => {
        dispatch({ type: 'socket_open' })
        ws.send(JSON.stringify(req))
      }
      ws.onmessage = (e: MessageEvent) => {
        if (typeof e.data !== 'string') {
          dispatch({ type: 'parse_error', raw: '<non-text frame>' })
          return
        }
        const result = parseFrame(e.data)
        if (result.ok) dispatch({ type: 'frame', frame: result.frame })
        else dispatch({ type: 'parse_error', raw: result.raw })
      }
      ws.onerror = () => dispatch({ type: 'socket_error', message: 'WebSocket connection error' })
      ws.onclose = () => dispatch({ type: 'socket_closed' })
    },
    [teardown],
  )

  // Replays a canned session through the same reducer — no backend, no quota.
  const startDemo = useCallback(() => {
    teardown()
    dispatch({ type: 'start' })
    dispatch({ type: 'socket_open' })
    demoSession.forEach((frame, i) => {
      const t = setTimeout(() => dispatch({ type: 'frame', frame }), DEMO_STEP_MS * (i + 1))
      timersRef.current.push(t)
    })
  }, [teardown])

  const reset = useCallback(() => {
    teardown()
    dispatch({ type: 'reset' })
  }, [teardown])

  return { state, start, startDemo, reset }
}
