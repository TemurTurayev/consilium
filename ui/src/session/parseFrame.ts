import type { InboundFrame } from '../protocol'

export type ParseResult =
  | { ok: true; frame: InboundFrame }
  | { ok: false; raw: string }

/**
 * Parse one inbound WebSocket text frame. Any JSON object with a string `type`
 * is forwarded — even an unrecognized tag — so a future Rust variant degrades to
 * the reducer's "unknown" branch instead of being dropped here. Malformed JSON,
 * or a value without a string `type`, is reported as `{ ok: false, raw }`.
 */
export function parseFrame(raw: string): ParseResult {
  let value: unknown
  try {
    value = JSON.parse(raw)
  } catch {
    return { ok: false, raw }
  }
  if (
    typeof value === 'object' &&
    value !== null &&
    'type' in value &&
    typeof (value as { type: unknown }).type === 'string'
  ) {
    return { ok: true, frame: value as InboundFrame }
  }
  return { ok: false, raw }
}
