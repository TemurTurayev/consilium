import { useState, type FormEvent } from 'react'
import type { SessionRequest } from '../protocol'
import { StartRunForm } from '../components/StartRunForm'
import type { SessionState } from '../session/reducer'
import { PatientCard } from './PatientCard'
import { SeatCard } from './SeatCard'
import type { SeatId } from './sprites'
import { deriveSeats } from './tableState'

interface Props {
  state: SessionState
  onStart: (req: SessionRequest) => void
  onDemo: () => void
  onCancel: () => void
  onPause: () => void
  onResume: () => void
  /** Doesn't take effect mid-agent-call — the conductor reads it at the next
   * decision point, so the UI copy says "noted", never "sent". */
  onInterject: (text: string) => void
}

const SEATS: { id: SeatId; name: string; role: string; slot: 'top' | 'left' | 'right' | 'bottom-left' }[] = [
  { id: 'claude', name: 'Claude', role: 'lead · conductor', slot: 'top' },
  { id: 'codex', name: 'Codex', role: 'builder · worker', slot: 'left' },
  { id: 'gemini', name: 'Gemini', role: 'reviewer', slot: 'right' },
  { id: 'grok', name: 'Grok', role: 'builder · worker', slot: 'bottom-left' },
]

/** The flagship view: the run rendered as a medical council around an
 * operating table, with the task as the "patient". Shares the same
 * `SessionState` (and start/demo/cancel callbacks) as the Run view — no
 * second socket, just a different lens on the same live state. */
export function TableView({ state, onStart, onDemo, onCancel, onPause, onResume, onInterject }: Props) {
  const { seats, patient, paused, operatorNote } = deriveSeats(state)
  const running = state.phase === 'running'
  const socketOpen = state.connection === 'open'
  const [note, setNote] = useState('')

  function handleInterject(e: FormEvent) {
    e.preventDefault()
    const trimmed = note.trim()
    if (!trimmed || !socketOpen) return
    onInterject(trimmed)
    setNote('')
  }

  return (
    <div className="table-view">
      <StartRunForm onStart={onStart} onDemo={onDemo} onCancel={onCancel} disabled={running} />
      <div className="scene">
        <div className="scene__table">
          <PatientCard patient={patient} paused={paused} />
        </div>
        {SEATS.map(({ id, name, role, slot }) => (
          <SeatCard key={id} seat={seats[id]} name={name} role={role} slot={slot} paused={paused} />
        ))}
        {(running || operatorNote) && (
          <div className="scene__operator">
            {operatorNote && (
              <div className="operator-note">
                <span className="operator-note__badge">Your note</span>
                <span className="operator-note__text">{operatorNote}</span>
                {running && <span className="operator-note__hint">queued for next decision</span>}
              </div>
            )}
            {running && (
              <div className="operator-strip">
                <div className="operator-strip__controls">
                  {paused ? (
                    <button className="btn btn--primary" type="button" onClick={onResume}>
                      Resume
                    </button>
                  ) : (
                    <button className="btn btn--ghost" type="button" onClick={onPause}>
                      Pause
                    </button>
                  )}
                  <button className="btn btn--danger" type="button" onClick={onCancel}>
                    Stop
                  </button>
                </div>
                <form className="operator-strip__interject" onSubmit={handleInterject}>
                  <input
                    className="field__input"
                    value={note}
                    onChange={(e) => setNote(e.target.value)}
                    placeholder="Guide the team at its next decision…"
                    disabled={!socketOpen}
                    aria-label="Add guidance for the team"
                  />
                  <button
                    className="btn btn--ghost"
                    type="submit"
                    disabled={!socketOpen || note.trim().length === 0}
                  >
                    Add guidance
                  </button>
                </form>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  )
}
