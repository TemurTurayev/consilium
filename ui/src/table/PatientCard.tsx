import type { Patient, Verdict } from './tableState'

const PHASE_LABEL: Record<Patient['phase'], string> = {
  idle: 'Awaiting admission',
  running: 'In session',
  done: 'Post-op',
  errored: 'Complications',
}

const VERDICT_LABEL: Record<NonNullable<Verdict>, string> = {
  completed: 'Stable — discharged',
  halted: 'Halted mid-procedure',
  failed: 'Complications',
  cancelled: 'Procedure aborted',
}

/** The centrepiece of the table: the task-as-patient. Shows run phase, files
 * touched so far, and — once the run has a terminal frame — a verdict badge. */
export function PatientCard({ patient }: { patient: Patient }) {
  return (
    <div className="patient">
      <div className="patient__figure" aria-hidden="true" />
      <div className="patient__body">
        <span className="patient__title">{patient.taskKnown ? 'Patient on the table' : 'No patient yet'}</span>
        <span className="patient__phase">{PHASE_LABEL[patient.phase]}</span>
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
