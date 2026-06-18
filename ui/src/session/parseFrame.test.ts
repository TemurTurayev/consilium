import { describe, it, expect } from 'vitest'
import { parseFrame } from './parseFrame'

describe('parseFrame', () => {
  it('parses a live AgentEvent frame', () => {
    const r = parseFrame('{"type":"message","text":"hi"}')
    expect(r.ok).toBe(true)
    if (r.ok) expect(r.frame.type).toBe('message')
  })

  it('parses a control frame', () => {
    const r = parseFrame('{"type":"run_complete","completed":[1],"halted":null,"failed":null}')
    expect(r.ok).toBe(true)
    if (r.ok) expect(r.frame.type).toBe('run_complete')
  })

  it('rejects malformed JSON', () => {
    const r = parseFrame('not json{')
    expect(r.ok).toBe(false)
    if (!r.ok) expect(r.raw).toBe('not json{')
  })

  it('rejects values without a string type', () => {
    expect(parseFrame('{"foo":1}').ok).toBe(false)
    expect(parseFrame('{"type":42}').ok).toBe(false)
    expect(parseFrame('[1,2,3]').ok).toBe(false)
    expect(parseFrame('"bare"').ok).toBe(false)
    expect(parseFrame('null').ok).toBe(false)
  })

  it('forwards an unknown type tag for forward-compat', () => {
    const r = parseFrame('{"type":"future_variant","x":1}')
    expect(r.ok).toBe(true)
    if (r.ok) expect(r.frame.type).toBe('future_variant')
  })
})
