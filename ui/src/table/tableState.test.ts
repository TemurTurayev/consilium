import { describe, it, expect } from 'vitest'
import { initialState, sessionReducer, type SessionState } from '../session/reducer'
import { demoSession } from '../session/demoSession'
import type { InboundFrame } from '../protocol'
import { deriveSeats } from './tableState'

/** Replays frames through the real reducer, mirroring session/reducer.test.ts,
 * so seat derivation is tested against authentic `SessionState` shapes rather
 * than hand-rolled fixtures. */
function replay(frames: InboundFrame[], from: SessionState = initialState): SessionState {
  return frames.reduce((s, frame) => sessionReducer(s, { type: 'frame', frame }), from)
}

describe('deriveSeats', () => {
  it('every seat is absent and the patient is unknown before any run', () => {
    const { seats, patient, paused, operatorNote } = deriveSeats(initialState)
    for (const id of ['claude', 'codex', 'gemini', 'grok'] as const) {
      expect(seats[id]).toEqual({ id, status: 'absent', model: null, lastMessage: null, toolName: null, active: false })
    }
    expect(patient).toEqual({ taskKnown: false, filesChanged: 0, phase: 'idle', verdict: null })
    expect(paused).toBe(false)
    expect(operatorNote).toBeNull()
  })

  it('a started run marks the patient known even with zero frames yet', () => {
    const started = sessionReducer(initialState, { type: 'start' })
    expect(deriveSeats(started).patient.taskKnown).toBe(true)
  })

  it('session_started makes a seat idle and active, leaving other seats absent', () => {
    const s = replay([{ type: 'session_started', session_id: 's1', provider: 'claude', model: 'opus' }])
    const { seats } = deriveSeats(s)
    expect(seats.claude).toMatchObject({ status: 'idle', model: 'opus', active: true })
    expect(seats.codex.status).toBe('absent')
    expect(seats.gemini.status).toBe('absent')
    expect(seats.grok.status).toBe('absent')
  })

  it('grok is always absent — the protocol has no fourth provider today', () => {
    const s = replay(demoSession)
    expect(deriveSeats(s).seats.grok).toEqual({
      id: 'grok',
      status: 'absent',
      model: null,
      lastMessage: null,
      toolName: null,
      active: false,
    })
  })

  it('thinking sets the active seat to thinking without touching lastMessage', () => {
    const s = replay([
      { type: 'session_started', session_id: 's1', provider: 'claude', model: null },
      { type: 'message', text: 'earlier line' },
      { type: 'thinking', text: 'pondering…' },
    ])
    const seat = deriveSeats(s).seats.claude
    expect(seat.status).toBe('thinking')
    expect(seat.lastMessage).toBe('earlier line')
  })

  it('message sets speaking and records the (truncated) text', () => {
    const s = replay([
      { type: 'session_started', session_id: 's1', provider: 'claude', model: null },
      { type: 'message', text: 'hello from the attending' },
    ])
    expect(deriveSeats(s).seats.claude).toMatchObject({ status: 'speaking', lastMessage: 'hello from the attending' })
  })

  it('truncates messages over 140 chars with an ellipsis, keeping length 140', () => {
    const long = 'x'.repeat(200)
    const s = replay([
      { type: 'session_started', session_id: 's1', provider: 'codex', model: null },
      { type: 'message', text: long },
    ])
    const msg = deriveSeats(s).seats.codex.lastMessage
    expect(msg).not.toBeNull()
    expect(msg).toHaveLength(140)
    expect(msg?.endsWith('…')).toBe(true)
    expect(msg?.slice(0, 139)).toBe('x'.repeat(139))
  })

  it('a message at exactly the limit is left untouched', () => {
    const exact = 'y'.repeat(140)
    const s = replay([
      { type: 'session_started', session_id: 's1', provider: 'codex', model: null },
      { type: 'message', text: exact },
    ])
    expect(deriveSeats(s).seats.codex.lastMessage).toBe(exact)
  })

  it('tool_call sets working and records the tool name', () => {
    const s = replay([
      { type: 'session_started', session_id: 's1', provider: 'codex', model: null },
      { type: 'tool_call', name: 'edit', detail: 'src/a.rs' },
    ])
    expect(deriveSeats(s).seats.codex).toMatchObject({ status: 'working', toolName: 'edit' })
  })

  it('file_changed sets working without clobbering the last tool name', () => {
    const s = replay([
      { type: 'session_started', session_id: 's1', provider: 'codex', model: null },
      { type: 'tool_call', name: 'edit', detail: 'src/a.rs' },
      { type: 'file_changed', path: 'src/a.rs' },
    ])
    expect(deriveSeats(s).seats.codex).toMatchObject({ status: 'working', toolName: 'edit' })
  })

  it('file_changed with no prior tool_call still moves the seat to working', () => {
    const s = replay([
      { type: 'session_started', session_id: 's1', provider: 'codex', model: null },
      { type: 'file_changed', path: 'src/a.rs' },
    ])
    expect(deriveSeats(s).seats.codex).toMatchObject({ status: 'working', toolName: null })
  })

  it('a later session_started moves "active" but freezes the prior seat\'s status', () => {
    const s = replay([
      { type: 'session_started', session_id: 's1', provider: 'claude', model: null },
      { type: 'message', text: 'plan' },
      { type: 'session_started', session_id: 's2', provider: 'codex', model: null },
    ])
    const { seats } = deriveSeats(s)
    expect(seats.claude).toMatchObject({ status: 'speaking', lastMessage: 'plan', active: false })
    expect(seats.codex).toMatchObject({ status: 'idle', active: true })
  })

  it('events before any session_started are dropped (no active seat yet)', () => {
    const s = replay([{ type: 'message', text: 'nobody is listening' }])
    const { seats } = deriveSeats(s)
    for (const id of ['claude', 'codex', 'gemini', 'grok'] as const) {
      expect(seats[id].status).toBe('absent')
    }
  })

  it('files changed counts distinct paths from state.files', () => {
    const s = replay([
      { type: 'session_started', session_id: 's1', provider: 'codex', model: null },
      { type: 'file_changed', path: 'a.rs' },
      { type: 'file_changed', path: 'a.rs' },
      { type: 'file_changed', path: 'b.rs' },
    ])
    expect(deriveSeats(s).patient.filesChanged).toBe(2)
  })

  it('verdict is completed for a clean run_complete', () => {
    const s = replay([{ type: 'run_complete', completed: [1], halted: null, failed: null }])
    expect(deriveSeats(s).patient.verdict).toBe('completed')
  })

  it('verdict is halted when the supervisor halted the run', () => {
    const s = replay([{ type: 'run_complete', completed: [1], halted: 'budget exceeded', failed: null }])
    expect(deriveSeats(s).patient.verdict).toBe('halted')
  })

  it('verdict is failed when the conductor failed the run', () => {
    const s = replay([{ type: 'run_complete', completed: [], halted: null, failed: 'rework exhausted' }])
    expect(deriveSeats(s).patient.verdict).toBe('failed')
  })

  it('verdict is cancelled on run_cancelled, even before any terminal frame', () => {
    const s = replay([{ type: 'run_cancelled' }])
    expect(deriveSeats(s).patient.verdict).toBe('cancelled')
  })

  it('verdict is failed for an errored run with no terminal frame', () => {
    const s = replay([{ type: 'run_error', error: 'boom' }])
    expect(deriveSeats(s).patient).toMatchObject({ phase: 'errored', verdict: 'failed' })
  })

  it('verdict is null mid-run', () => {
    const s = replay([{ type: 'session_started', session_id: 's1', provider: 'claude', model: null }])
    expect(deriveSeats(s).patient.verdict).toBeNull()
  })

  it('paused mirrors state.paused', () => {
    const s = replay([{ type: 'paused' }])
    expect(deriveSeats(s).paused).toBe(true)
  })

  it('operatorNote exposes the most recent operator_note text', () => {
    const s = replay([
      { type: 'operator_note', text: 'first' },
      { type: 'operator_note', text: 'second' },
    ])
    expect(deriveSeats(s).operatorNote).toBe('second')
  })

  it('operatorNote stays null when none has arrived', () => {
    const s = replay([{ type: 'paused' }])
    expect(deriveSeats(s).operatorNote).toBeNull()
  })

  it('replays the full demo fixture to the expected final seat + patient state', () => {
    const s = replay(demoSession)
    const { seats, patient, paused, operatorNote } = deriveSeats(s)

    expect(seats.claude).toMatchObject({
      status: 'speaking',
      model: 'claude-opus-4-8',
      lastMessage: 'Plan: (1) add the parser, (2) wire it into the CLI, (3) cover it with tests.',
      active: false,
    })
    expect(seats.codex).toMatchObject({
      status: 'speaking',
      model: 'gpt-5.4',
      lastMessage: 'Implemented the parser and wired it into the CLI entrypoint.',
      active: false,
    })
    expect(seats.gemini).toMatchObject({
      status: 'working',
      model: 'gemini-3-pro-preview',
      toolName: 'shell',
      active: true,
    })
    expect(seats.grok.status).toBe('absent')

    expect(patient).toEqual({ taskKnown: true, filesChanged: 2, phase: 'done', verdict: 'completed' })
    // the mid-demo pause/note/resume beat resolves before the run ends, but
    // the note itself persists as "what was last said"
    expect(paused).toBe(false)
    expect(operatorNote).toBe('Prefer the simpler fix, please')
  })
})
