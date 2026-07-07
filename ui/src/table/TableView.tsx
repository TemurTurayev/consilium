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
}

const SEATS: { id: SeatId; name: string; role: string; slot: 'top' | 'left' | 'right' | 'bottom-right' }[] = [
  { id: 'claude', name: 'Claude', role: 'attending · conductor', slot: 'top' },
  { id: 'codex', name: 'Codex', role: 'surgeon · worker', slot: 'left' },
  { id: 'gemini', name: 'Gemini', role: 'radiologist · review', slot: 'right' },
  { id: 'grok', name: 'Grok', role: 'resident · worker', slot: 'bottom-right' },
]

/** The flagship view: the run rendered as a medical council around an
 * operating table, with the task as the "patient". Shares the same
 * `SessionState` (and start/demo/cancel callbacks) as the Run view — no
 * second socket, just a different lens on the same live state. */
export function TableView({ state, onStart, onDemo, onCancel }: Props) {
  const { seats, patient } = deriveSeats(state)
  const running = state.phase === 'running'

  return (
    <div className="table-view">
      <StartRunForm onStart={onStart} onDemo={onDemo} onCancel={onCancel} disabled={running} />
      <div className="scene">
        <div className="scene__table">
          <PatientCard patient={patient} />
        </div>
        {SEATS.map(({ id, name, role, slot }) => (
          <SeatCard key={id} seat={seats[id]} name={name} role={role} slot={slot} />
        ))}
      </div>
    </div>
  )
}
