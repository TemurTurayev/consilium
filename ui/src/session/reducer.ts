import type { AgentEvent, InboundFrame, Provider, RunComplete } from '../protocol'

export type ConnectionStatus = 'idle' | 'connecting' | 'open' | 'closed' | 'error'
export type Phase = 'idle' | 'running' | 'done' | 'errored'

export interface SessionInfo {
  sessionId: string
  provider: Provider
  model: string | null
}

export interface SessionState {
  phase: Phase
  connection: ConnectionStatus
  session: SessionInfo | null
  /** Append-only live log, in arrival order. */
  events: AgentEvent[]
  /** Running totals, summed across every `usage` frame. */
  usage: { inputTokens: number; outputTokens: number }
  /** Distinct `file_changed` paths. */
  files: string[]
  terminal: RunComplete | null
  /** Set when the run ended via `{"kind":"cancel"}` rather than completing —
   * a terminal state distinct from `terminal`, so `ResultPanel` can show
   * "Run cancelled" instead of the accepted-subtasks summary. */
  cancelled: boolean
  /** True from a `paused` event until the matching `resumed` event, or until
   * any terminal frame (`run_complete` / `run_cancelled` / `run_error`)
   * clears it — a run can't stay "paused" once it's over. */
  paused: boolean
  error: string | null
}

export const initialState: SessionState = {
  phase: 'idle',
  connection: 'idle',
  session: null,
  events: [],
  usage: { inputTokens: 0, outputTokens: 0 },
  files: [],
  terminal: null,
  cancelled: false,
  paused: false,
  error: null,
}

export type SessionAction =
  | { type: 'start' }
  | { type: 'socket_open' }
  | { type: 'frame'; frame: InboundFrame }
  | { type: 'parse_error'; raw: string }
  | { type: 'socket_closed' }
  | { type: 'socket_error'; message: string }
  | { type: 'reset' }

/** Pure fold — no React, no socket, no clock. Replaying a frame sequence through
 * this reproduces the exact UI state, which is what the unit tests assert. */
export function sessionReducer(state: SessionState, action: SessionAction): SessionState {
  switch (action.type) {
    case 'start':
      return { ...initialState, phase: 'running', connection: 'connecting' }
    case 'socket_open':
      return { ...state, connection: 'open' }
    case 'frame':
      return foldFrame(state, action.frame)
    case 'parse_error':
      // A stray frame after a clean completion shouldn't clobber 'done'
      // (mirrors socket_error).
      return {
        ...state,
        phase: state.phase === 'done' ? 'done' : 'errored',
        error: state.error ?? `unparseable frame: ${action.raw}`,
      }
    case 'socket_error':
      // A socket error after a clean completion is benign — keep `done`.
      return {
        ...state,
        connection: 'error',
        phase: state.phase === 'done' ? 'done' : 'errored',
        error: state.error ?? action.message,
      }
    case 'socket_closed':
      return { ...state, connection: 'closed' }
    case 'reset':
      return initialState
    default:
      return state
  }
}

function foldFrame(state: SessionState, frame: InboundFrame): SessionState {
  switch (frame.type) {
    case 'run_complete':
      // Clears any stale mid-run `error` (e.g. a transient `error` frame the
      // run recovered from) — otherwise ResultPanel's error-first check would
      // show "Run error" over a run that actually finished cleanly. Also
      // clears a stale `paused` — a finished run can't still be on hold.
      return { ...state, phase: 'done', terminal: frame, error: null, paused: false }
    case 'run_cancelled':
      return { ...state, phase: 'done', cancelled: true, error: null, paused: false }
    case 'run_error':
      return { ...state, phase: 'errored', error: frame.error, paused: false }
    case 'error':
      return { ...state, phase: 'errored', error: frame.error }
    case 'paused':
      return { ...appendEvent(state, frame), paused: true }
    case 'resumed':
      return { ...appendEvent(state, frame), paused: false }
    default:
      return appendEvent(state, frame)
  }
}

// Every AgentEvent (and any unknown future tag) is appended to the live stream;
// session_started / usage / file_changed also feed their summary slices.
function appendEvent(state: SessionState, event: AgentEvent): SessionState {
  let { session, usage, files } = state
  switch (event.type) {
    case 'session_started':
      session = {
        sessionId: event.session_id,
        provider: event.provider,
        model: event.model,
      }
      break
    case 'usage':
      usage = {
        inputTokens: usage.inputTokens + event.input_tokens,
        outputTokens: usage.outputTokens + event.output_tokens,
      }
      break
    case 'file_changed':
      files = files.includes(event.path) ? files : [...files, event.path]
      break
    default:
      break
  }
  return { ...state, session, usage, files, events: [...state.events, event] }
}
