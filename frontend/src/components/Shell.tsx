import { useState } from 'react'
import { NavLink, Outlet, useLocation, useNavigate } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { api } from '../api'
import { Icon, type IconName } from './Icon'
import { getTheme, applyTheme, type Theme } from '../theme'

type NavItem = { to: string; label: string; icon: IconName; end?: boolean }

const groups: Array<{ label: string; items: NavItem[] }> = [
  {
    label: 'Workspace',
    items: [
      { to: '/', label: 'Overview', icon: 'grid', end: true },
      { to: '/catalog', label: 'Catalog', icon: 'database' },
    ],
  },
  {
    label: 'Data',
    items: [
      { to: '/query', label: 'SQL editor', icon: 'terminal' },
      { to: '/data', label: 'Load & transfer', icon: 'upload' },
      { to: '/jobs', label: 'Jobs', icon: 'clock' },
    ],
  },
  {
    label: 'Governance',
    items: [{ to: '/lineage', label: 'Lineage', icon: 'lineage' }],
  },
  {
    label: 'Analytics',
    items: [{ to: '/dashboards', label: 'Dashboards', icon: 'chart' }],
  },
  {
    label: 'Platform',
    items: [
      { to: '/apps', label: 'Apps', icon: 'grid' },
      { to: '/health', label: 'Health', icon: 'pulse' },
    ],
  },
]

const titles: Record<string, string> = {
  '/': 'Overview',
  '/catalog': 'Catalog',
  '/query': 'SQL editor',
  '/data': 'Load & transfer',
  '/jobs': 'Jobs',
  '/apps': 'Platform apps',
  '/lineage': 'Lineage',
  '/dashboards': 'Dashboards',
  '/health': 'Health',
}

/** The app detail route carries its own id, so the header can be specific. */
function titleFor(pathname: string): string {
  const app = pathname.match(/^\/apps\/([^/]+)$/)
  if (app) return `${app[1]} — app detail`
  return titles[pathname] ?? 'Nova'
}

function navClass({ isActive }: { isActive: boolean }) {
  return [
    'flex items-center gap-2.5 px-3 py-2 rounded-[10px] text-sm transition-colors',
    isActive
      ? 'bg-accent/15 text-text border border-accent/30'
      : 'text-text-dim hover:text-text hover:bg-bg-soft border border-transparent',
  ].join(' ')
}

export default function Shell() {
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const location = useLocation()
  const navigate = useNavigate()
  const [q, setQ] = useState('')
  const [theme, setThemeState] = useState<Theme>(getTheme())
  const title = titleFor(location.pathname)

  function toggleTheme() {
    const next: Theme = theme === 'dark' ? 'light' : 'dark'
    setThemeState(next)
    applyTheme(next)
  }

  return (
    <div className="h-full grid grid-cols-[250px_1fr]">
      <aside className="border-r border-border bg-bg-elev/50 backdrop-blur flex flex-col min-h-0">
        <div className="p-5 flex items-center gap-3">
          <div className="w-8 h-8 rounded-[11px] bg-gradient-to-b from-accent to-accent-deep shadow-lg shadow-accent/20" />
          <div className="leading-tight">
            <div className="font-bold tracking-tight">stardelt</div>
            <div className="text-[11px] text-text-mute -mt-0.5">Nova</div>
          </div>
        </div>

        <div className="px-3 pb-3">
          <div className="card px-3 py-2 flex items-center gap-2">
            <Icon name="database" className="w-4 h-4 text-accent" />
            <div className="min-w-0">
              <div className="text-[10px] uppercase tracking-widest text-text-mute">Warehouse</div>
              <div className="text-sm truncate">{me.data?.warehouse ?? 'warehouse'}</div>
            </div>
            {me.data?.demo && (
              <span className="badge ml-auto border-amber-400/40 bg-amber-400/10 text-amber-300">demo</span>
            )}
          </div>
        </div>

        <nav className="px-3 pb-4 overflow-auto flex-1">
          {groups.map(group => (
            <div key={group.label} className="mb-4">
              <div className="label px-3 mb-1.5">{group.label}</div>
              <div className="space-y-1">
                {group.items.map(item => (
                  <NavLink key={item.to} to={item.to} end={item.end} className={navClass}>
                    <Icon name={item.icon} className="w-4 h-4" />
                    {item.label}
                  </NavLink>
                ))}
              </div>
            </div>
          ))}
        </nav>

        <div className="p-3 border-t border-border">
          <div className="flex items-center gap-2.5 px-2 py-1.5">
            <div className="w-7 h-7 rounded-full bg-accent/20 border border-accent/30 grid place-items-center text-xs font-semibold text-accent">
              {(me.data?.user ?? '?').slice(0, 1).toUpperCase()}
            </div>
            <div className="min-w-0">
              <div className="text-sm truncate">{me.data?.user ?? 'loading…'}</div>
              <div className="text-[11px] text-text-mute">{me.data?.auth_mode ?? ''}</div>
            </div>
          </div>
        </div>
      </aside>

      <div className="flex flex-col min-w-0 min-h-0">
        <header className="h-14 shrink-0 border-b border-border bg-bg/30 backdrop-blur flex items-center gap-4 px-6">
          <div className="text-sm font-medium">{title}</div>
          <form
            className="ml-auto relative"
            onSubmit={e => {
              e.preventDefault()
              if (q.trim()) navigate(`/catalog?q=${encodeURIComponent(q.trim())}`)
            }}
          >
            <Icon name="search" className="w-4 h-4 absolute left-3 top-1/2 -translate-y-1/2 text-text-mute" />
            <input
              value={q}
              onChange={e => setQ(e.target.value)}
              placeholder="Search catalog…"
              className="input pl-9 w-64 py-1.5"
            />
          </form>
          <NavLink to="/data" className="btn-primary">
            <Icon name="plus" className="w-4 h-4" />
            Load data
          </NavLink>
          <button
            onClick={toggleTheme}
            title={theme === 'dark' ? 'Switch to light' : 'Switch to dark'}
            className="btn-ghost px-2.5 py-2"
            aria-label="Toggle theme"
          >
            {theme === 'dark' ? <Icon name="spark" className="w-4 h-4" /> : <Icon name="pulse" className="w-4 h-4" />}
          </button>
        </header>

        <main className="flex-1 min-h-0 overflow-auto p-6">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
