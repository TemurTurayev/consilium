import type { ReactNode } from 'react'
import type { AgentEvent } from '../protocol'

interface RowProps {
  kind: string
  badge: string
  provider?: string
  children: ReactNode
}

function Row({ kind, badge, provider, children }: RowProps) {
  const rail = provider ? ` row--p-${provider}` : ''
  return (
    <div className={`row row--${kind}${rail}`}>
      <span className="row__badge">{badge}</span>
      <span className="row__body">{children}</span>
    </div>
  )
}

/** Renders one AgentEvent distinctly by tag. The default branch keeps an unknown
 * future variant from crashing the UI. */
export function EventRow({ event }: { event: AgentEvent }) {
  switch (event.type) {
    case 'session_started':
      return (
        <Row kind="session" badge="SESSION" provider={event.provider}>
          <span className="row__strong">{event.provider}</span>
          {event.model && <span className="row__dim"> · {event.model}</span>}
          <span className="row__faint"> · {event.session_id}</span>
        </Row>
      )
    case 'thinking':
      return (
        <Row kind="thinking" badge="THINKING">
          <span className="row__italic">{event.text}</span>
        </Row>
      )
    case 'message':
      return (
        <Row kind="message" badge="MESSAGE">
          {event.text}
        </Row>
      )
    case 'tool_call':
      return (
        <Row kind="tool" badge="TOOL">
          <span className="row__mono row__strong">{event.name}</span>
          {event.detail && <span className="row__mono row__dim"> {event.detail}</span>}
        </Row>
      )
    case 'file_changed':
      return (
        <Row kind="file" badge="FILE">
          <span className="row__mono">{event.path}</span>
        </Row>
      )
    case 'usage':
      return (
        <Row kind="usage" badge="USAGE">
          <span className="row__dim">
            ↑ {event.input_tokens} · ↓ {event.output_tokens} tokens
          </span>
        </Row>
      )
    case 'completed':
      return (
        <Row kind="completed" badge="DONE">
          {event.result ?? <span className="row__dim">completed</span>}
        </Row>
      )
    case 'failed':
      return (
        <Row kind="failed" badge="FAIL">
          <span className="row__err">{event.error}</span>
        </Row>
      )
    case 'paused':
      return (
        <Row kind="system" badge="PAUSED">
          <span className="row__dim">Council paused by the chief physician.</span>
        </Row>
      )
    case 'resumed':
      return (
        <Row kind="system" badge="RESUMED">
          <span className="row__dim">Council resumed.</span>
        </Row>
      )
    case 'operator_note':
      return (
        <Row kind="operator" badge="OPERATOR">
          {event.text}
        </Row>
      )
    default:
      return (
        <Row kind="unknown" badge="EVENT">
          <span className="row__dim">{JSON.stringify(event)}</span>
        </Row>
      )
  }
}
