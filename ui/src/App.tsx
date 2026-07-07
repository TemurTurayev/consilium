import { useState } from 'react'
import { useSession } from './session/useSession'
import { Sidebar, type View } from './components/Sidebar'
import { StartRunForm } from './components/StartRunForm'
import { StatusPill } from './components/StatusPill'
import { SessionHeader } from './components/SessionHeader'
import { UsageBadge } from './components/UsageBadge'
import { EventStream } from './components/EventStream'
import { ResultPanel } from './components/ResultPanel'
import { QuotaDashboard } from './components/QuotaDashboard'
import { ProvidersView } from './components/ProvidersView'
import { SettingsView } from './components/SettingsView'
import { TableView } from './table/TableView'

export function App() {
  const { state, start, startDemo, cancel, pause, resume, interject, reset } = useSession()
  const running = state.phase === 'running'
  const [view, setView] = useState<View>('run')

  return (
    <div className="shell">
      <Sidebar view={view} onSelect={setView} />
      <div className="app">
        <header className="app__bar">
          <span className="app__bar-title">{VIEW_TITLE[view]}</span>
          <div className="app__meta">
            <UsageBadge usage={state.usage} />
            <StatusPill phase={state.phase} connection={state.connection} />
          </div>
        </header>

        <main className="app__main">
          {view === 'run' && (
            <>
              <StartRunForm onStart={start} onDemo={startDemo} onCancel={cancel} disabled={running} />
              <SessionHeader session={state.session} />
              <EventStream events={state.events} />
              <ResultPanel
                terminal={state.terminal}
                cancelled={state.cancelled}
                error={state.error}
                phase={state.phase}
                onReset={reset}
              />
            </>
          )}
          {view === 'table' && (
            <TableView
              state={state}
              onStart={start}
              onDemo={startDemo}
              onCancel={cancel}
              onPause={pause}
              onResume={resume}
              onInterject={interject}
            />
          )}
          {view === 'usage' && <QuotaDashboard active={view === 'usage'} />}
          {view === 'providers' && <ProvidersView />}
          {view === 'settings' && <SettingsView />}
        </main>
      </div>
    </div>
  )
}

const VIEW_TITLE: Record<View, string> = {
  run: 'Run',
  table: 'Table',
  usage: 'Usage',
  providers: 'Providers',
  settings: 'Settings',
}
