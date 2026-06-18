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
      return { ...state, phase: 'done', terminal: frame }
    case 'run_error':
      return { ...state, phase: 'errored', error: frame.error }
    case 'error':
      return { ...state, phase: 'errored', error: frame.error }
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
