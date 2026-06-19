import { useQuota } from '../dashboard/useQuota'
import { formatTokens, formatWindow, maxTotal, quotaRows } from '../dashboard/quotaView'

/** The Usage tab: per-provider token totals over the rolling window, polled
 * from `GET /api/quota`. `active` gates polling so it idles when hidden. */
export function QuotaDashboard({ active }: { active: boolean }) {
  const { state, refresh } = useQuota(active)
  const { snapshot, error, loading } = state
  const rows = snapshot ? quotaRows(snapshot) : []
  const peak = maxTotal(rows)

  return (
    <section className="dash">
      <div className="dash__head">
        <h2 className="dash__title">
          Usage{snapshot ? <span className="dash__window"> · last {formatWindow(snapshot.window_secs)}</span> : ''}
        </h2>
        <button className="dash__refresh" onClick={refresh} disabled={loading} title="Refresh">
          {loading ? '…' : '↻'}
        </button>
      </div>

      {error && (
        <p className="dash__error">
          Couldn’t reach the server: {error}. Is <code>consilium serve</code> running?
        </p>
      )}
      {!snapshot && !error && <p className="dash__empty">Loading usage…</p>}

      {snapshot && (
        <ul className="dash__rows">
          {rows.map((row) => (
            <li key={row.provider} className={`dash__row dash__row--${row.provider}`}>
              <span className={`dash__dot dot--${row.provider}`} aria-hidden="true" />
              <span className="dash__name">{row.label}</span>
              <span className="dash__bar">
                <span className="dash__fill" style={{ width: `${Math.round((row.total / peak) * 100)}%` }} />
              </span>
              <span className="dash__nums">
                <span className="dash__total">{formatTokens(row.total)}</span>
                <span className="dash__io">
                  ↑{formatTokens(row.input)} ↓{formatTokens(row.output)}
                </span>
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}
