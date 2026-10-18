import { useEffect, useState } from 'react'
import { NavLink, Outlet, useLocation, useNavigate } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { api } from '../api'
import { Icon, type IconName } from './Icon'
import { getTheme, applyTheme, type Theme } from '../theme'
import { isAdmin, roleLabel } from '../roles'
import ErrorBoundary from './ErrorBoundary'

type NavItem = { to: string; label: string; icon: IconName; end?: boolean }
/** `admin: true` hides the whole group from non-admins (see `roles.ts`). */
type NavGroup = { label: string; items: NavItem[]; admin?: boolean }

const groups: NavGroup[] = [
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
    items: [
      { to: '/dashboards', label: 'Dashboards', icon: 'chart' },
      { to: '/notebooks', label: 'Notebooks', icon: 'notebook' },
    ],
  },
  {
    // Operating the platform is not a data-engineering task: an analyst has no use for
    // the service inventory, the workload list or the settings page, and the noise
    // makes the parts they do use harder to find.
    label: 'Admin',
    admin: true,
    items: [
      { to: '/apps', label: 'Apps', icon: 'grid' },
      { to: '/health', label: 'Health', icon: 'pulse' },
      { to: '/settings', label: 'Settings', icon: 'sliders' },
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
  '/notebooks': 'Notebooks',
  '/health': 'Health',
  '/settings': 'Settings',
}

/** The app detail route carries its own id, so the header can be specific. */
function titleFor(pathname: string): string {
  const app = pathname.match(/^\/apps\/([^/]+)$/)
  if (app) return `${app[1]} — app detail`
  // Anything else that is not a known route is the catch-all, and says so.
  return titles[pathname] ?? 'Not found'
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
  const [theme, setThemeState] = useState<Theme>(getTheme)
  const title = titleFor(location.pathname)
  const admin = isAdmin(me.data)
  const role = roleLabel(me.data)

  useEffect(() => {
    document.title = title === 'Nova' ? 'stardelt Nova' : `${title} · stardelt Nova`
  }, [title])

  function toggleTheme() {
    const next: Theme = theme === 'dark' ? 'light' : 'dark'
    setThemeState(next)
    applyTheme(next)
  }

  return (
    <div className="h-full grid grid-cols-[250px_1fr]">
      <aside className="border-r border-border bg-bg-elev/50 backdrop-blur flex flex-col min-h-0">
        <div className="p-5 flex items-center gap-2.5">
          {/* The brand asset, not a stand-in: `logo-wordmark.svg` is the same file the
              favicon and the docs use, so the product and its documentation cannot drift. */}
          <img src="/logo-wordmark.svg" alt="stardelt" className="h-7 w-auto" />
          <div className="text-[11px] text-text-mute leading-tight">Nova</div>
        </div>

        <div className="px-3 pb-3">
          <div className="card px-3 py-2 flex items-center gap-2">
            <Icon name="database" className="w-4 h-4 text-accent" />
            <div className="min-w-0">
              <div className="text-[10px] uppercase tracking-widest text-text-mute">Warehouse</div>
              <div className="text-sm truncate">{me.data?.warehouse ?? 'warehouse'}</div>
            </div>
            {me.data?.demo && (
              <span className="badge ml-auto border-amber-400/40 bg-amber-400/10 text-warn">demo</span>
            )}
          </div>
        </div>

        <nav className="px-3 pb-4 overflow-auto flex-1">
          {groups
            .filter(group => !group.admin || admin)
            .map(group => (
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
              <div className="text-sm truncate">{me.data?.user ?? (me.isError ? 'unknown user' : 'loading…')}</div>
              <div className="text-[11px] text-text-mute flex items-center gap-1.5">
                {role && (
                  <span
                    className={[
                      'badge px-1.5 py-0 text-[10px]',
                      admin
                        ? 'border-accent/40 bg-accent/10 text-accent'
                        : 'border-border bg-bg-soft text-text-mute',
                    ].join(' ')}
                  >
                    {role}
                  </span>
                )}
                {me.data?.auth_mode ?? ''}
              </div>
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
              aria-label="Search catalog"
              className="input pl-9 w-64 py-1.5"
            />
          </form>
          <NavLink to="/data" className="btn-primary">
            <Icon name="plus" className="w-4 h-4" />
            Load data
          </NavLink>
          {me.data?.auth_mode === 'oidc' && (
            <div className="flex items-center gap-2 pl-2 border-l border-border">
              <div className="text-right leading-tight min-w-0" title={me.data.email ?? me.data.sub ?? ''}>
                <div className="text-sm truncate max-w-[12rem]">{me.data.user}</div>
                {me.data.is_admin && <div className="text-[11px] text-text-mute">admin</div>}
              </div>
              {/* A plain form POST: the backend clears the session and 303s to the IdP's logout. */}
              <form method="post" action="/api/auth/logout">
                <button type="submit" className="btn-ghost px-2.5 py-1.5 text-sm">
                  Sign out
                </button>
              </form>
            </div>
          )}
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
          <ErrorBoundary key={location.pathname}>
            <Outlet />
          </ErrorBoundary>
        </main>
      </div>
    </div>
  )
}
