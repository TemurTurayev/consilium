export type View = 'run' | 'table' | 'usage' | 'providers' | 'settings'

const ITEMS: { id: View; label: string }[] = [
  { id: 'run', label: 'Build' },
  { id: 'table', label: 'Live team' },
  { id: 'usage', label: 'Usage' },
  { id: 'providers', label: 'Providers' },
  { id: 'settings', label: 'Settings' },
]

interface Props {
  view: View
  onSelect: (view: View) => void
}

/** Left nav for the app shell. Replaces the old header tabs so the header row
 * can stay slim (brand + live status) while the section list grows. */
export function Sidebar({ view, onSelect }: Props) {
  return (
    <nav className="sidebar" aria-label="Sections">
      <div className="brand">
        <span className="brand__name">Consilium</span>
        <span className="brand__dots" aria-hidden="true">
          <i className="dot dot--claude" />
          <i className="dot dot--codex" />
          <i className="dot dot--gemini" />
        </span>
      </div>
      <ul className="sidebar__list">
        {ITEMS.map((item) => (
          <li key={item.id}>
            <button
              className={view === item.id ? 'sidebar__item sidebar__item--on' : 'sidebar__item'}
              onClick={() => onSelect(item.id)}
            >
              {item.label}
            </button>
          </li>
        ))}
      </ul>
    </nav>
  )
}
