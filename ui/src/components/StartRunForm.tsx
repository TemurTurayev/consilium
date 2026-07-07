import { useState, type FormEvent } from 'react'
import type { SessionRequest } from '../protocol'

interface Props {
  onStart: (req: SessionRequest) => void
  onDemo: () => void
  onCancel: () => void
  disabled: boolean
}

interface TauriDialog {
  dialog?: { open: (opts: { directory: boolean }) => Promise<string | string[] | null> }
}

function tauriDialog(): TauriDialog['dialog'] | null {
  const w = window as unknown as { __TAURI__?: TauriDialog }
  return w.__TAURI__?.dialog ?? null
}

export function StartRunForm({ onStart, onDemo, onCancel, disabled }: Props) {
  const [task, setTask] = useState('')
  const [context, setContext] = useState('')
  const [cwd, setCwd] = useState('')

  const canStart = task.trim().length > 0 && !disabled
  const dialog = tauriDialog()

  function handleSubmit(e: FormEvent) {
    e.preventDefault()
    if (!canStart) return
    onStart({
      kind: 'conduct',
      task: task.trim(),
      context: context.trim(),
      cwd: cwd.trim() || null,
    })
  }

  async function handleBrowse() {
    if (!dialog) return
    const picked = await dialog.open({ directory: true })
    if (typeof picked === 'string') setCwd(picked)
  }

  return (
    <form className="form" onSubmit={handleSubmit}>
      <label className="field">
        <span className="field__label">Task</span>
        <textarea
          className="field__input"
          rows={3}
          value={task}
          onChange={(e) => setTask(e.target.value)}
          placeholder="Describe what the council should do…"
          disabled={disabled}
        />
      </label>
      <div className="form__row">
        <label className="field">
          <span className="field__label">
            Context <span className="field__hint">optional</span>
          </span>
          <input
            className="field__input"
            value={context}
            onChange={(e) => setContext(e.target.value)}
            placeholder="Extra context for the run"
            disabled={disabled}
          />
        </label>
        <label className="field">
          <span className="field__label">
            Working dir <span className="field__hint">optional</span>
          </span>
          <div className="field__row">
            <input
              className="field__input"
              value={cwd}
              onChange={(e) => setCwd(e.target.value)}
              placeholder="defaults to the server's cwd"
              disabled={disabled}
            />
            {dialog && (
              <button className="btn btn--ghost" type="button" onClick={() => void handleBrowse()} disabled={disabled}>
                Browse…
              </button>
            )}
          </div>
        </label>
      </div>
      <div className="form__actions">
        {disabled ? (
          <button className="btn btn--danger" type="button" onClick={onCancel}>
            Stop
          </button>
        ) : (
          <button className="btn btn--primary" type="submit" disabled={!canStart}>
            Conduct
          </button>
        )}
        <button className="btn btn--ghost" type="button" onClick={onDemo} disabled={disabled}>
          Demo run
        </button>
      </div>
    </form>
  )
}
