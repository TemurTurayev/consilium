import type { ReactNode } from 'react'
import { SPRITES } from './sprites'
import { SeatSprite } from './SeatSprite'
import type { Seat } from './tableState'

interface Props {
  seat: Seat
  name: string
  role: string
  /** Positions this seat around the table ellipse (see index.css). */
  slot: 'top' | 'left' | 'right' | 'bottom-left'
  /** The council is on hold — dims the seat as part of the scene's "on hold" treatment. */
  paused?: boolean
}

function statusLine(seat: Seat): ReactNode {
  switch (seat.status) {
    case 'thinking':
      return (
        <span className="seat__dots" aria-label="thinking">
          <i />
          <i />
          <i />
        </span>
      )
    case 'working':
      return seat.toolName ?? 'working'
    case 'speaking':
      return 'speaking'
    case 'idle':
      return 'idle'
    case 'absent':
      return 'away'
  }
}

/** One council member's seat: sprite, name/role, a one-line status, and — for
 * the currently active seat only — a speech bubble with its last message. */
export function SeatCard({ seat, name, role, slot, paused = false }: Props) {
  const showBubble = seat.active && seat.lastMessage !== null
  return (
    <div
      className={`seat seat--${seat.id} seat--${seat.status} seat--slot-${slot}${seat.active ? ' seat--active' : ''}${paused ? ' seat--paused' : ''}`}
    >
      {showBubble && <div className="seat__bubble">{seat.lastMessage}</div>}
      <SeatSprite sprite={SPRITES[seat.id]} />
      <div className="seat__label">
        <span className="seat__name">{name}</span>
        <span className="seat__role">{role}</span>
        <span className="seat__status">{statusLine(seat)}</span>
      </div>
    </div>
  )
}
