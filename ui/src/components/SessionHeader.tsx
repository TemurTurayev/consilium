import type { SessionState } from '../session/reducer'

interface Props {
  session: SessionState['session']
}

export function SessionHeader({ session }: Props) {
  if (!session) return null
  return (
    <div className="session">
      <span className={`tag tag--${session.provider}`}>{session.provider}</span>
      {session.model && <span className="session__model">{session.model}</span>}
      <span className="session__id" title="session id">
        {session.sessionId}
      </span>
    </div>
  )
}
