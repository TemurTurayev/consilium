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
      <div className="form__intro">
        <p className="form__eyebrow">Multi-agent build</p>
        <h1 className="form__title">Give the team one clear outcome.</h1>
        <p className="form__lede">
          Consilium plans the work, delegates it to coding agents, runs your checks, and asks another agent to review.
        </p>
        <div className="agent-route" aria-label="Plan, build, verify, review">
          <span className="agent-route__step agent-route__step--plan">Plan</span>
          <i aria-hidden="true" />
          <span className="agent-route__step agent-route__step--build">Build</span>
          <i aria-hidden="true" />
          <span className="agent-route__step agent-route__step--verify">Verify</span>
          <i aria-hidden="true" />
          <span className="agent-route__step agent-route__step--review">Review</span>
        </div>
      </div>
      <label className="field">
        <span className="field__label">What should the team finish?</span>
        <textarea
          className="field__input"
          rows={3}
          value={task}
          onChange={(e) => setTask(e.target.value)}
          placeholder="For example: fix the login redirect and add a regression test"
          disabled={disabled}
        />
      </label>
      {!disabled && task.length === 0 && (
        <div className="form__examples" aria-label="Example tasks">
          <span>Try an example:</span>
          <button type="button" onClick={() => setTask('Explain the riskiest part of this codebase and propose a safer design')}>
            assess a codebase
          </button>
          <button type="button" onClick={() => setTask('Fix the failing tests without changing public behavior')}>
            fix failing tests
          </button>
        </div>
      )}
      <details className="form__advanced">
        <summary>Context and working folder</summary>
        <div className="form__row">
          <label className="field">
            <span className="field__label">
              Context <span className="field__hint">optional</span>
            </span>
            <input
              className="field__input"
              value={context}
              onChange={(e) => setContext(e.target.value)}
              placeholder="Constraints or architecture notes"
              disabled={disabled}
            />
          </label>
          <label className="field">
            <span className="field__label">
              Working folder <span className="field__hint">optional</span>
            </span>
            <div className="field__row">
              <input
                className="field__input"
                value={cwd}
                onChange={(e) => setCwd(e.target.value)}
                placeholder="Uses the server folder by default"
                disabled={disabled}
              />
              {dialog && (
                <button className="btn btn--ghost" type="button" onClick={() => void handleBrowse()} disabled={disabled}>
                  Choose…
                </button>
              )}
            </div>
          </label>
        </div>
      </details>
      <div className="form__actions">
        {disabled ? (
          <button className="btn btn--danger" type="button" onClick={onCancel}>
            Stop
          </button>
        ) : (
          <button className="btn btn--primary" type="submit" disabled={!canStart}>
            Start team run
          </button>
        )}
        <button className="btn btn--ghost" type="button" onClick={onDemo} disabled={disabled}>
          Try the demo
        </button>
        {!disabled && <span className="form__demo-note">Demo uses no provider quota</span>}
      </div>
    </form>
  )
}
