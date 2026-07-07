// Pure derivation of the "council around the table" view from `SessionState`.
// No React, no DOM — unit-testable by replaying frames through the real
// `sessionReducer` and asserting on the result (mirrors dashboard/quotaView.ts).
import type { AgentEvent } from '../protocol'
import type { Phase, SessionState } from '../session/reducer'
import type { SeatId } from './sprites'

export const SEAT_ORDER: readonly SeatId[] = ['claude', 'codex', 'gemini', 'grok']

export type SeatStatus = 'absent' | 'idle' | 'thinking' | 'speaking' | 'working'

export interface Seat {
  id: SeatId
  status: SeatStatus
  /** From this seat's own `session_started`; null until it has one. */
  model: string | null
  /** Last `message` text attributed to this seat, truncated. Persists across
   * later thinking/working turns so a seat that goes quiet keeps showing
   * what it last said. */
  lastMessage: string | null
  /** Last `tool_call` name attributed to this seat. */
  toolName: string | null
  /** True for the seat whose `session_started` fired most recently — the one
   * currently receiving attributed events. At most one seat is active. */
  active: boolean
}

export type Verdict = 'completed' | 'halted' | 'failed' | 'cancelled' | null

export interface Patient {
  /** A run has been started this session (even if no frames have arrived
   * yet) — `SessionState` doesn't echo the task text back, so this is the
   * closest thing to "is there a patient on the table". */
  taskKnown: boolean
  filesChanged: number
  phase: Phase
  verdict: Verdict
}

export interface TableState {
  seats: Record<SeatId, Seat>
  patient: Patient
  /** Mirrors `SessionState.paused` — the council is on hold. */
  paused: boolean
  /** Text of the most recent `operator_note` event, or null if the chief
   * physician hasn't said anything this run. Persists across later events
   * (there's no "note cleared" frame), so it reads as "what was last said"
   * rather than "what's currently pending". */
  operatorNote: string | null
}

const MESSAGE_TRUNCATE_LEN = 140

function truncate(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max - 1)}…` : text
}

function absentSeat(id: SeatId): Seat {
  return { id, status: 'absent', model: null, lastMessage: null, toolName: null, active: false }
}

interface Acc {
  seats: Record<SeatId, Seat>
  active: SeatId | null
}

function withSeat(acc: Acc, id: SeatId, patch: Partial<Seat>): Acc {
  return { active: acc.active, seats: { ...acc.seats, [id]: { ...acc.seats[id], ...patch } } }
}

/**
 * Folds one `AgentEvent` into the accumulator, attributing thinking/message/
 * tool_call/file_changed events to the currently "active" seat.
 *
 * Protocol limitation: `AgentEvent` only tags `session_started` with a
 * `provider` — the other event types carry no provider of their own. We
 * treat whichever provider's `session_started` fired most recently as
 * "active" and attribute every following thinking/message/tool_call/
 * file_changed event to it. That matches the conductor → worker → reviewer
 * handoff every backend adapter follows today (see `session/demoSession.ts`),
 * but would misattribute events from two providers genuinely running
 * concurrently — the protocol has no way to distinguish that today.
 */
function foldEvent(acc: Acc, event: AgentEvent): Acc {
  switch (event.type) {
    case 'session_started': {
      const id = event.provider as SeatId
      return withSeat({ seats: acc.seats, active: id }, id, { status: 'idle', model: event.model })
    }
    case 'thinking':
      return acc.active ? withSeat(acc, acc.active, { status: 'thinking' }) : acc
    case 'message':
      return acc.active
        ? withSeat(acc, acc.active, { status: 'speaking', lastMessage: truncate(event.text, MESSAGE_TRUNCATE_LEN) })
        : acc
    case 'tool_call':
      return acc.active ? withSeat(acc, acc.active, { status: 'working', toolName: event.name }) : acc
    case 'file_changed':
      // A file write follows a tool_call; keep the seat "working" but don't
      // touch `toolName` — the tool_call that caused it already set it.
      return acc.active ? withSeat(acc, acc.active, { status: 'working' }) : acc
    default:
      return acc
  }
}

function deriveVerdict(state: SessionState): Verdict {
  if (state.cancelled) return 'cancelled'
  if (state.terminal) {
    if (state.terminal.failed) return 'failed'
    if (state.terminal.halted) return 'halted'
    return 'completed'
  }
  // A run that errored out without a terminal frame (run_error / a socket
  // error mid-run) reads as "failed" on the patient card too.
  if (state.phase === 'errored') return 'failed'
  return null
}

function derivePatient(state: SessionState): Patient {
  return {
    taskKnown: state.phase !== 'idle',
    filesChanged: state.files.length,
    phase: state.phase,
    verdict: deriveVerdict(state),
  }
}

/** Finds the most recent `operator_note` text in the event stream, scanning
 * from the end — cheap for the handful of events a run produces. */
function deriveOperatorNote(events: AgentEvent[]): string | null {
  for (let i = events.length - 1; i >= 0; i--) {
    const event = events[i]
    if (event.type === 'operator_note') return event.text
  }
  return null
}

/** Derives per-seat status + the patient summary from the live session state.
 * Replaying `state.events` is cheap (a handful of frames per run) and keeps
 * this a pure function of `SessionState`, so it can be called straight from
 * a render without any memoization. */
export function deriveSeats(state: SessionState): TableState {
  const initial: Acc = {
    seats: {
      claude: absentSeat('claude'),
      codex: absentSeat('codex'),
      gemini: absentSeat('gemini'),
      grok: absentSeat('grok'),
    },
    active: null,
  }

  const folded = state.events.reduce(foldEvent, initial)

  const seats = SEAT_ORDER.reduce<Record<SeatId, Seat>>(
    (acc, id) => {
      acc[id] = { ...folded.seats[id], active: id === folded.active }
      return acc
    },
    {} as Record<SeatId, Seat>,
  )

  return {
    seats,
    patient: derivePatient(state),
    paused: state.paused,
    operatorNote: deriveOperatorNote(state.events),
  }
}
