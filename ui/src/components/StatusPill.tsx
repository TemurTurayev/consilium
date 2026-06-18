import type { ConnectionStatus, Phase } from '../session/reducer'

interface Props {
  phase: Phase
  connection: ConnectionStatus
}

const PHASE_LABEL: Record<Phase, string> = {
  idle: 'idle',
  running: 'running',
  done: 'complete',
  errored: 'error',
}

export function StatusPill({ phase, connection }: Props) {
  return (
    <span className={`pill pill--${phase}`} title={`connection: ${connection}`}>
      <i className="pill__dot" aria-hidden="true" />
      {PHASE_LABEL[phase]}
    </span>
  )
}
