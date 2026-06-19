import { describe, expect, it } from 'vitest'
import type { QuotaSnapshot } from '../protocol'
import { formatTokens, formatWindow, maxTotal, quotaRows } from './quotaView'

const snap: QuotaSnapshot = {
  window_secs: 18000,
  claude: { input_tokens: 1000, output_tokens: 500 },
  codex: { input_tokens: 200, output_tokens: 0 },
  gemini: { input_tokens: 0, output_tokens: 0 },
}

describe('quotaRows', () => {
  it('maps providers to rows with totals in a stable order', () => {
    const rows = quotaRows(snap)
    expect(rows.map((r) => r.provider)).toEqual(['claude', 'codex', 'gemini'])
    expect(rows[0]).toMatchObject({ label: 'Claude', input: 1000, output: 500, total: 1500 })
    expect(rows[1].total).toBe(200)
    expect(rows[2].total).toBe(0)
  })
})

describe('formatTokens', () => {
  it('formats compactly and drops trailing .0', () => {
    expect(formatTokens(0)).toBe('0')
    expect(formatTokens(999)).toBe('999')
    expect(formatTokens(1500)).toBe('1.5k')
    expect(formatTokens(2000)).toBe('2k')
    expect(formatTokens(1_500_000)).toBe('1.5M')
  })
})

describe('formatWindow', () => {
  it('humanizes whole hours/minutes', () => {
    expect(formatWindow(18000)).toBe('5h')
    expect(formatWindow(90 * 60)).toBe('90m')
    expect(formatWindow(45)).toBe('45s')
  })
})

describe('maxTotal', () => {
  it('returns the peak total and never zero', () => {
    expect(maxTotal(quotaRows(snap))).toBe(1500)
    const idle: QuotaSnapshot = {
      window_secs: 18000,
      claude: { input_tokens: 0, output_tokens: 0 },
      codex: { input_tokens: 0, output_tokens: 0 },
      gemini: { input_tokens: 0, output_tokens: 0 },
    }
    expect(maxTotal(quotaRows(idle))).toBe(1)
  })
})
