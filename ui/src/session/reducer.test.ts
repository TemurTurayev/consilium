import { describe, it, expect } from 'vitest'
import { initialState, sessionReducer, type SessionState } from './reducer'
import { demoSession } from './demoSession'
import type { AgentEvent, InboundFrame, ServerFrame } from '../protocol'

function replay(frames: InboundFrame[], from: SessionState = initialState): SessionState {
  return frames.reduce((s, frame) => sessionReducer(s, { type: 'frame', frame }), from)
}

describe('sessionReducer', () => {
  it('starts from a clean initial state', () => {
    expect(initialState.phase).toBe('idle')
    expect(initialState.events).toHaveLength(0)
    expect(initialState.usage).toEqual({ inputTokens: 0, outputTokens: 0 })
    expect(initialState.paused).toBe(false)
  })

  it('start resets a prior run and enters running/connecting', () => {
    const dirty = replay([
      { type: 'message', text: 'old' },
      { type: 'usage', input_tokens: 9, output_tokens: 9 },
    ])
    const s = sessionReducer(dirty, { type: 'start' })
    expect(s.phase).toBe('running')
    expect(s.connection).toBe('connecting')
    expect(s.events).toHaveLength(0)
    expect(s.usage).toEqual({ inputTokens: 0, outputTokens: 0 })
  })

  it('socket_open marks the connection open', () => {
    const s = sessionReducer({ ...initialState, connection: 'connecting' }, { type: 'socket_open' })
    expect(s.connection).toBe('open')
  })

  it('session_started populates session and appends to the stream', () => {
    const s = replay([{ type: 'session_started', session_id: 's1', provider: 'claude', model: 'opus' }])
    expect(s.session).toEqual({ sessionId: 's1', provider: 'claude', model: 'opus' })
    expect(s.events).toHaveLength(1)
  })

  it('accumulates usage across multiple usage frames', () => {
    const s = replay([
      { type: 'usage', input_tokens: 10, output_tokens: 2 },
      { type: 'usage', input_tokens: 5, output_tokens: 3 },
    ])
    expect(s.usage).toEqual({ inputTokens: 15, outputTokens: 5 })
    expect(s.events).toHaveLength(2)
  })

  it('dedups file_changed paths but keeps every event in the stream', () => {
    const s = replay([
      { type: 'file_changed', path: 'a.rs' },
      { type: 'file_changed', path: 'a.rs' },
      { type: 'file_changed', path: 'b.rs' },
    ])
    expect(s.files).toEqual(['a.rs', 'b.rs'])
    expect(s.events).toHaveLength(3)
  })

  it('appends thinking/message/tool_call/completed/failed in order', () => {
    const evs: AgentEvent[] = [
      { type: 'thinking', text: 't' },
      { type: 'message', text: 'm' },
      { type: 'tool_call', name: 'edit', detail: 'x' },
      { type: 'completed', result: 'ok' },
      { type: 'failed', error: 'boom' },
    ]
    const s = replay(evs)
    expect(s.events.map((e) => e.type)).toEqual(['thinking', 'message', 'tool_call', 'completed', 'failed'])
  })

  it('run_complete sets terminal + done with the real wire shape', () => {
    const term: ServerFrame = { type: 'run_complete', completed: [1, 2], halted: null, failed: null }
    const s = replay([term])
    expect(s.phase).toBe('done')
    expect(s.terminal).toEqual({ type: 'run_complete', completed: [1, 2], halted: null, failed: null })
  })

  it('run_error and server error frames set errored', () => {
    expect(replay([{ type: 'run_error', error: 'x' }]).phase).toBe('errored')
    expect(replay([{ type: 'run_error', error: 'x' }]).error).toBe('x')
    expect(replay([{ type: 'error', error: 'bad request' }]).error).toBe('bad request')
  })

  it('parse_error and socket_error move to errored without throwing', () => {
    const pe = sessionReducer(initialState, { type: 'parse_error', raw: '{oops' })
    expect(pe.phase).toBe('errored')
    expect(pe.error).toContain('{oops')
    const se = sessionReducer({ ...initialState, phase: 'running' }, { type: 'socket_error', message: 'down' })
    expect(se.phase).toBe('errored')
    expect(se.connection).toBe('error')
  })

  it('a parse error after completion does not clobber done', () => {
    const done = replay([{ type: 'run_complete', completed: [1], halted: null, failed: null }])
    const s = sessionReducer(done, { type: 'parse_error', raw: 'junk' })
    expect(s.phase).toBe('done')
  })

  it('a clean close after run_complete stays done', () => {
    const done = replay([{ type: 'run_complete', completed: [1], halted: null, failed: null }])
    const closed = sessionReducer(done, { type: 'socket_closed' })
    expect(closed.phase).toBe('done')
    expect(closed.connection).toBe('closed')
  })

  it('socket_error after completion does not clobber done', () => {
    const done = replay([{ type: 'run_complete', completed: [1], halted: null, failed: null }])
    const s = sessionReducer(done, { type: 'socket_error', message: 'late' })
    expect(s.phase).toBe('done')
    expect(s.connection).toBe('error')
  })

  it('forwards an unknown event type to the stream without crashing', () => {
    const unknown = { type: 'future_variant', blob: 1 } as unknown as InboundFrame
    const s = replay([unknown])
    expect(s.events).toHaveLength(1)
    expect(s.phase).toBe('idle')
  })

  it('replays a full recorded session into the expected aggregate', () => {
    const s = replay([
      { type: 'session_started', session_id: 's', provider: 'codex', model: null },
      { type: 'thinking', text: 'plan' },
      { type: 'tool_call', name: 'write', detail: 'out.txt' },
      { type: 'file_changed', path: 'out.txt' },
      { type: 'usage', input_tokens: 100, output_tokens: 40 },
      { type: 'message', text: 'done' },
      { type: 'run_complete', completed: [1], halted: null, failed: null },
    ])
    expect(s.session?.provider).toBe('codex')
    expect(s.events).toHaveLength(6) // every AgentEvent, not the control frame
    expect(s.files).toEqual(['out.txt'])
    expect(s.usage).toEqual({ inputTokens: 100, outputTokens: 40 })
    expect(s.phase).toBe('done')
    expect(s.terminal?.completed).toEqual([1])
  })

  it('reset returns to the initial state', () => {
    const dirty = replay([{ type: 'message', text: 'x' }])
    expect(sessionReducer(dirty, { type: 'reset' })).toEqual(initialState)
  })

  it('run_cancelled sets a terminal cancelled state distinct from run_complete', () => {
    const s = replay([{ type: 'run_cancelled' }])
    expect(s.phase).toBe('done')
    expect(s.cancelled).toBe(true)
    expect(s.terminal).toBeNull()
  })

  it('run_complete clears a stale mid-run error', () => {
    const dirty = replay([{ type: 'error', error: 'transient' }])
    expect(dirty.error).toBe('transient')
    const s = replay([{ type: 'run_complete', completed: [1], halted: null, failed: null }], dirty)
    expect(s.error).toBeNull()
    expect(s.phase).toBe('done')
    expect(s.terminal?.completed).toEqual([1])
  })

  it('run_cancelled clears a stale mid-run error', () => {
    const dirty = replay([{ type: 'error', error: 'transient' }])
    const s = replay([{ type: 'run_cancelled' }], dirty)
    expect(s.error).toBeNull()
    expect(s.phase).toBe('done')
    expect(s.cancelled).toBe(true)
  })

  it('a clean close after run_cancelled stays done', () => {
    const cancelled = replay([{ type: 'run_cancelled' }])
    const closed = sessionReducer(cancelled, { type: 'socket_closed' })
    expect(closed.phase).toBe('done')
    expect(closed.cancelled).toBe(true)
  })

  it('the demo fixture replays to a clean completed run', () => {
    const s = replay(demoSession)
    expect(s.phase).toBe('done')
    expect(s.terminal?.completed).toEqual([1, 2, 3])
    expect(s.files).toEqual(['src/parser.rs', 'src/cli.rs'])
    expect(s.usage).toEqual({ inputTokens: 27642, outputTokens: 2950 })
    // last session_started wins (the gemini reviewer)
    expect(s.session?.provider).toBe('gemini')
    // the mid-demo pause/note/resume beat resolves before the run ends
    expect(s.paused).toBe(false)
  })

  it('the demo fixture carries an operator pause/note/resume beat, in order', () => {
    const s = replay(demoSession)
    const types = s.events.map((e) => e.type)
    const pausedIdx = types.indexOf('paused')
    const noteIdx = types.indexOf('operator_note')
    const resumedIdx = types.indexOf('resumed')
    expect(pausedIdx).toBeGreaterThan(-1)
    expect(noteIdx).toBe(pausedIdx + 1)
    expect(resumedIdx).toBe(noteIdx + 1)
    expect(s.events[noteIdx]).toEqual({ type: 'operator_note', text: 'Prefer the simpler fix, please' })
  })

  describe('paused', () => {
    it('sets the flag and appends to the stream', () => {
      const s = replay([{ type: 'paused' }])
      expect(s.paused).toBe(true)
      expect(s.events.map((e) => e.type)).toEqual(['paused'])
    })

    it('resumed clears the flag and appends to the stream', () => {
      const s = replay([{ type: 'paused' }, { type: 'resumed' }])
      expect(s.paused).toBe(false)
      expect(s.events.map((e) => e.type)).toEqual(['paused', 'resumed'])
    })

    it('operator_note appends to the stream without touching the paused flag', () => {
      const s = replay([{ type: 'paused' }, { type: 'operator_note', text: 'go simpler' }])
      expect(s.paused).toBe(true)
      expect(s.events).toHaveLength(2)
      expect(s.events[1]).toEqual({ type: 'operator_note', text: 'go simpler' })
    })

    it('run_complete clears a stale paused flag', () => {
      const s = replay([{ type: 'paused' }, { type: 'run_complete', completed: [1], halted: null, failed: null }])
      expect(s.paused).toBe(false)
      expect(s.phase).toBe('done')
    })

    it('run_cancelled clears a stale paused flag', () => {
      const s = replay([{ type: 'paused' }, { type: 'run_cancelled' }])
      expect(s.paused).toBe(false)
      expect(s.phase).toBe('done')
    })

    it('run_error clears a stale paused flag', () => {
      const s = replay([{ type: 'paused' }, { type: 'run_error', error: 'boom' }])
      expect(s.paused).toBe(false)
      expect(s.phase).toBe('errored')
    })
  })
})
