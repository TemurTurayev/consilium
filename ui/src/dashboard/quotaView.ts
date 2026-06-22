// Pure view helpers for the quota dashboard — no DOM, no fetch — so they're
// unit-testable in the node-env vitest suite (the impure fetch lives in
// `useQuota`).
import type { ProviderUsage, QuotaSnapshot } from '../protocol'

export type ProviderKey = 'claude' | 'codex' | 'gemini'

export interface QuotaRow {
  provider: ProviderKey
  label: string
  input: number
  output: number
  total: number
  /** Tokens are heuristic estimates (provider reports no usage, e.g. Gemini via agy). */
  estimated: boolean
}

const PROVIDERS: ProviderKey[] = ['claude', 'codex', 'gemini']
const LABELS: Record<ProviderKey, string> = {
  claude: 'Claude',
  codex: 'Codex',
  gemini: 'Gemini',
}

/** Flatten a snapshot into one display row per provider (stable order). */
export function quotaRows(snap: QuotaSnapshot): QuotaRow[] {
  return PROVIDERS.map((provider) => {
    const usage: ProviderUsage = snap[provider]
    return {
      provider,
      label: LABELS[provider],
      input: usage.input_tokens,
      output: usage.output_tokens,
      total: usage.input_tokens + usage.output_tokens,
      estimated: usage.estimated,
    }
  })
}

/** Compact token count: 1234 → "1.2k", 1_500_000 → "1.5M". */
export function formatTokens(n: number): string {
  if (n < 1000) return String(n)
  if (n < 1_000_000) return `${(n / 1000).toFixed(1).replace(/\.0$/, '')}k`
  return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`
}

/** Window seconds → human ("5h", "90m", "30s"). */
export function formatWindow(secs: number): string {
  if (secs > 0 && secs % 3600 === 0) return `${secs / 3600}h`
  if (secs > 0 && secs % 60 === 0) return `${secs / 60}m`
  return `${secs}s`
}

/** Largest row total, for scaling bars. Floored at 1 to avoid divide-by-zero
 * when every provider is idle. */
export function maxTotal(rows: QuotaRow[]): number {
  return Math.max(1, ...rows.map((r) => r.total))
}
