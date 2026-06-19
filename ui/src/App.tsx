import { useState } from 'react'
import { useSession } from './session/useSession'
import { StartRunForm } from './components/StartRunForm'
import { StatusPill } from './components/StatusPill'
import { SessionHeader } from './components/SessionHeader'
import { UsageBadge } from './components/UsageBadge'
import { EventStream } from './components/EventStream'
import { ResultPanel } from './components/ResultPanel'
import { QuotaDashboard } from './components/QuotaDashboard'

type View = 'session' | 'usage'

export function App() {
  const { state, start, startDemo, reset } = useSession()
  const running = state.phase === 'running'
  const [view, setView] = useState<View>('session')

  return (
    <div className="app">
      <header className="app__bar">
        <div className="brand">
          <span className="brand__name">consilium</span>
          <span className="brand__dots" aria-hidden="true">
            <i className="dot dot--claude" />
            <i className="dot dot--codex" />
            <i className="dot dot--gemini" />
          </span>
        </div>
        <nav className="app__nav">
          <button className={view === 'session' ? 'tab tab--on' : 'tab'} onClick={() => setView('session')}>
            Session
          </button>
          <button className={view === 'usage' ? 'tab tab--on' : 'tab'} onClick={() => setView('usage')}>
            Usage
          </button>
        </nav>
        <div className="app__meta">
          <UsageBadge usage={state.usage} />
          <StatusPill phase={state.phase} connection={state.connection} />
        </div>
      </header>

      <main className="app__main">
        {view === 'session' ? (
          <>
            <StartRunForm onStart={start} onDemo={startDemo} disabled={running} />
            <SessionHeader session={state.session} />
            <EventStream events={state.events} />
            <ResultPanel terminal={state.terminal} error={state.error} phase={state.phase} onReset={reset} />
          </>
        ) : (
          <QuotaDashboard active={view === 'usage'} />
        )}
      </main>
    </div>
  )
}
