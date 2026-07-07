import { useEffect, useState } from 'react'
import type { ConfigSummary, VersionInfo } from '../protocol'
import { apiUrl } from '../runtime'

interface Loaded {
  config: ConfigSummary | null
  version: VersionInfo | null
  error: string | null
}

const initial: Loaded = { config: null, version: null, error: null }

/** Read-only summary of the council + build version, fetched once on mount
 * from `GET /api/config` and `GET /api/version`. Nothing here is editable —
 * config changes happen in the server's config file. */
export function SettingsView() {
  const [state, setState] = useState<Loaded>(initial)

  useEffect(() => {
    let cancelled = false
    async function load() {
      try {
        const [configUrl, versionUrl] = await Promise.all([apiUrl('/api/config'), apiUrl('/api/version')])
        const [configRes, versionRes] = await Promise.all([fetch(configUrl), fetch(versionUrl)])
        if (!configRes.ok) throw new Error(`GET /api/config: HTTP ${configRes.status}`)
        if (!versionRes.ok) throw new Error(`GET /api/version: HTTP ${versionRes.status}`)
        const config = (await configRes.json()) as ConfigSummary
        const version = (await versionRes.json()) as VersionInfo
        if (!cancelled) setState({ config, version, error: null })
      } catch (err) {
        if (!cancelled) setState({ config: null, version: null, error: (err as Error).message })
      }
    }
    void load()
    return () => {
      cancelled = true
    }
  }, [])

  return (
    <section className="settings">
      <h2 className="settings__title">Settings</h2>

      {state.error && (
        <p className="settings__error">
          Couldn’t reach the server: {state.error}. Is <code>consilium serve</code> running?
        </p>
      )}
      {!state.config && !state.error && <p className="settings__empty">Loading config…</p>}

      {state.config && (
        <dl className="settings__grid">
          <dt>Conductor</dt>
          <dd>{state.config.conductor}</dd>
          <dt>Workers</dt>
          <dd>{state.config.workers.join(', ') || '—'}</dd>
          <dt>Reviewer</dt>
          <dd>{state.config.reviewer}</dd>
          <dt>Chairman</dt>
          <dd>{state.config.chairman}</dd>
          <dt>Supervisor</dt>
          <dd>{state.config.supervisor}</dd>
          <dt>Cross-family review</dt>
          <dd>{state.config.cross_family_review ? 'on' : 'off'}</dd>
          <dt>Budget</dt>
          <dd>{state.config.budget_secs != null ? `${state.config.budget_secs}s` : 'unlimited'}</dd>
          <dt>Config path</dt>
          <dd className="settings__mono">{state.config.config_path ?? 'built-in defaults'}</dd>
        </dl>
      )}

      {state.version && <p className="settings__version">consilium v{state.version.version}</p>}
    </section>
  )
}
