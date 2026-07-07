import type { RunComplete } from '../protocol'
import type { Phase } from '../session/reducer'

interface Props {
  terminal: RunComplete | null
  cancelled: boolean
  error: string | null
  phase: Phase
  onReset: () => void
}

export function ResultPanel({ terminal, cancelled, error, phase, onReset }: Props) {
  if (phase !== 'done' && phase !== 'errored') return null

  if (error) {
    return (
      <div className="banner banner--err">
        <div className="banner__text">
          <strong>Run error</strong>
          <span>{error}</span>
        </div>
        <button className="btn btn--ghost" onClick={onReset}>
          New run
        </button>
      </div>
    )
  }

  if (cancelled) {
    return (
      <div className="banner banner--warn">
        <div className="banner__text">
          <strong>Run cancelled</strong>
        </div>
        <button className="btn btn--ghost" onClick={onReset}>
          New run
        </button>
      </div>
    )
  }

  if (terminal) {
    const { completed, halted, failed } = terminal
    const tone = halted || failed ? 'banner--warn' : 'banner--ok'
    return (
      <div className={`banner ${tone}`}>
        <div className="banner__text">
          <strong>Run complete</strong>
          <span>
            {completed.length} subtask{completed.length === 1 ? '' : 's'} accepted
            {completed.length ? `: ${completed.join(', ')}` : ''}
          </span>
          {halted && <span className="banner__note">halted: {halted}</span>}
          {failed && <span className="banner__note">failed: {failed}</span>}
        </div>
        <button className="btn btn--ghost" onClick={onReset}>
          New run
        </button>
      </div>
    )
  }

  return null
}
