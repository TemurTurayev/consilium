import { useCallback, useState } from 'react'
import type { AuthState, DoctorReport, ProviderStatus } from '../protocol'
import { apiUrl } from '../runtime'

type Status = { kind: 'idle' } | { kind: 'probing' } | { kind: 'error'; message: string } | { kind: 'done'; report: DoctorReport }

const BADGE_LABEL: Record<AuthState, string> = {
  ready: 'ready',
  needs_login: 'needs login',
  cli_missing: 'CLI missing',
  down: 'down',
}

/** `GET /api/doctor` spawns each provider's real CLI server-side to probe
 * liveness, so it takes real seconds and is never auto-polled — the user asks
 * for it explicitly with the "Check providers" button. */
export function ProvidersView() {
  const [status, setStatus] = useState<Status>({ kind: 'idle' })

  const check = useCallback(async () => {
    setStatus({ kind: 'probing' })
    try {
      const url = await apiUrl('/api/doctor')
      const res = await fetch(url)
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
      const report = (await res.json()) as DoctorReport
      setStatus({ kind: 'done', report })
    } catch (err) {
      setStatus({ kind: 'error', message: (err as Error).message })
    }
  }, [])

  return (
    <section className="providers">
      <div className="providers__head">
        <h2 className="providers__title">Providers</h2>
        <button className="btn btn--primary" onClick={() => void check()} disabled={status.kind === 'probing'}>
          {status.kind === 'probing' ? 'Checking…' : 'Check providers'}
        </button>
      </div>

      {status.kind === 'idle' && (
        <p className="providers__empty">Spawns each provider's CLI to check auth — takes a few seconds.</p>
      )}
      {status.kind === 'probing' && <p className="providers__empty">Probing provider CLIs…</p>}
      {status.kind === 'error' && (
        <p className="providers__error">
          Couldn’t reach the server: {status.message}. Is <code>consilium serve</code> running?
        </p>
      )}
      {status.kind === 'done' && (
        <ul className="providers__list">
          {status.report.providers.map((p) => (
            <ProviderCard key={p.provider} status={p} />
          ))}
        </ul>
      )}
    </section>
  )
}

function ProviderCard({ status }: { status: ProviderStatus }) {
  return (
    <li className={`providers__card providers__card--${status.provider}`}>
      <div className="providers__card-head">
        <span className={`tag tag--${status.provider}`}>{status.provider}</span>
        <span className={`badge badge--${status.state}`}>{BADGE_LABEL[status.state]}</span>
      </div>
      {status.detail && <p className="providers__detail">{status.detail}</p>}
      {status.hint && <code className="providers__hint">{status.hint}</code>}
    </li>
  )
}
