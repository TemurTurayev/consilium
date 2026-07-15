import type { Patient, Verdict } from './tableState'

const PHASE_LABEL: Record<Patient['phase'], string> = {
  idle: 'Ready for a task',
  running: 'Work in progress',
  done: 'Run finished',
  errored: 'Run needs attention',
}

const VERDICT_LABEL: Record<NonNullable<Verdict>, string> = {
  completed: 'Completed',
  halted: 'Stopped by review',
  failed: 'Checks failed',
  cancelled: 'Run cancelled',
}

/** The centrepiece of the table: the task-as-patient. Shows run phase, files
 * touched so far, and — once the run has a terminal frame — a verdict badge. */
export function PatientCard({ patient, paused }: { patient: Patient; paused: boolean }) {
  return (
    <div className="patient">
      <div className="patient__figure" aria-hidden="true" />
      <div className="patient__body">
        <span className="patient__title">{patient.taskKnown ? 'Current task' : 'No task yet'}</span>
        <span className="patient__phase">{PHASE_LABEL[patient.phase]}</span>
        {paused && <span className="badge patient__paused-pill">Team paused</span>}
        <span className="patient__files">
          {patient.filesChanged} file{patient.filesChanged === 1 ? '' : 's'} touched
        </span>
        {patient.verdict && (
          <span className={`badge patient__verdict patient__verdict--${patient.verdict}`}>
            {VERDICT_LABEL[patient.verdict]}
          </span>
        )}
      </div>
    </div>
  )
}
