import { useEffect, useRef } from 'react'
import type { AgentEvent } from '../protocol'
import { EventRow } from './EventRow'

interface Props {
  events: AgentEvent[]
}

export function EventStream({ events }: Props) {
  const endRef = useRef<HTMLDivElement | null>(null)
  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: 'smooth', block: 'end' })
  }, [events.length])

  if (events.length === 0) {
    return <div className="stream stream--empty">No activity yet. Start a team run or try the demo.</div>
  }
  return (
    <div className="stream">
      {/* index keys are safe: `events` is strictly append-only (see sessionReducer). */}
      {events.map((event, i) => (
        <EventRow key={i} event={event} />
      ))}
      <div ref={endRef} />
    </div>
  )
}
