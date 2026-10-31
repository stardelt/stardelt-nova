import { useEffect, useRef, useState } from 'react'
import { NavLink, Outlet, useLocation, useNavigate } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { api } from '../api'
import { Icon, type IconName } from './Icon'
import { applyTheme, useTheme, type Theme } from '../theme'
import { isAdmin } from '../roles'
import ErrorBoundary from './ErrorBoundary'
import UserMenu from './UserMenu'

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
      { to: '/connections', label: 'Connections', icon: 'key' },
      { to: '/queries', label: 'Query log', icon: 'table' },
      { to: '/jobs', label: 'Jobs', icon: 'clock' },
      { to: '/pipelines', label: 'Pipelines', icon: 'play' },
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
      { to: '/admin/catalogs', label: 'Catalogs', icon: 'database' },
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
  '/connections': 'Connections',
  '/queries': 'Query log',
  '/jobs': 'Jobs',
  '/pipelines': 'Pipelines',
  '/apps': 'Platform apps',
  '/admin/catalogs': 'Catalogs',
  '/lineage': 'Lineage',
  '/dashboards': 'Dashboards',
  '/notebooks': 'Notebooks',
  '/health': 'Health',
  '/settings': 'Settings',
  '/preferences': 'Preferences',
  '/profile': 'Profile',
}

/** The app detail route carries its own id, so the header can be specific. */
function titleFor(pathname: string): string {
  const app = pathname.match(/^\/apps\/([^/]+)$/)
  if (app) return `${app[1]} — app detail`
  // A pipeline's page is named after the pipeline, so the tab says which one.
  const pipeline = pathname.match(/^\/pipelines\/[^/]+\/([^/]+)$/)
  if (pipeline) {
    let name = pipeline[1]
    try {
      name = decodeURIComponent(name)
    } catch {
      // A malformed escape is shown as it arrived; nothing here builds a URL from it.
    }
    return `${name} · Pipelines`
  }
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


/**
 * The header's app switcher. `/apps` is an operator page (workloads, images), so a
 * data engineer has no other way to reach Superset, Airflow or JupyterHub without
 * knowing the hostname. The list is status-free on purpose: liveness is operator
 * information, and this control is for everyone.
 */
function AppSwitcher() {
  const [open, setOpen] = useState(false)
  const wrapper = useRef<HTMLDivElement>(null)
  const button = useRef<HTMLButtonElement>(null)
  const location = useLocation()
  const links = useQuery({ queryKey: ['app-links'], queryFn: api.links, staleTime: 60_000 })

  // Moving to another page is a new context; the menu should not survive it.
  useEffect(() => setOpen(false), [location.pathname])

  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        setOpen(false)
        button.current?.focus()
      }
    }
    const onClick = (e: MouseEvent) => {
      if (wrapper.current && !wrapper.current.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('keydown', onKey)
    document.addEventListener('mousedown', onClick)
    return () => {
      document.removeEventListener('keydown', onKey)
      document.removeEventListener('mousedown', onClick)
    }
  }, [open])

  const apps = links.data ?? []
  // Nothing to offer (no base domain, or every app is down to zero entries): no button.
  if (apps.length === 0) return null

  return (
    <div ref={wrapper} className="relative">
      <button
        ref={button}
        type="button"
        className="btn-ghost py-1.5 text-sm"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen(v => !v)}
      >
        <Icon name="grid" className="w-4 h-4" />
        Apps
      </button>
      {open && (
        <div
          role="menu"
          aria-label="Open another app"
          className="absolute right-0 top-full mt-1 z-50 min-w-[14rem] rounded-[12px] border border-border bg-bg-elev shadow-2xl p-1.5"
        >
          {apps.map(app => (
            <a
              key={app.id}
              role="menuitem"
              href={app.external_url}
              target="_blank"
              rel="noreferrer"
              className="flex items-center justify-between gap-6 px-3 py-2 rounded-[8px] text-sm hover:bg-bg-soft"
            >
              <span>{app.name}</span>
              <span className="text-[11px] text-text-mute">{app.category}</span>
            </a>
          ))}
        </div>
      )}
    </div>
  )
}

export default function Shell() {
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const location = useLocation()
  const navigate = useNavigate()
  const [q, setQ] = useState('')
  const theme = useTheme()
  const title = titleFor(location.pathname)
  const admin = isAdmin(me.data)
  const [navOpen, setNavOpen] = useState(false)
  useEffect(() => { setNavOpen(false) }, [location.pathname])

  useEffect(() => {
    document.title = title === 'Nova' ? 'stardelt Nova' : `${title} · stardelt Nova`
  }, [title])

  function toggleTheme() {
    const next: Theme = theme === 'dark' ? 'light' : 'dark'
    applyTheme(next)
    // The toggle and the Preferences page write the same two places; a missing state
    // store only means the choice stays in this browser.
    void api.putSetting('theme', next).catch(() => {})
  }

  return (
    <div className="h-full grid grid-cols-1 md:grid-cols-[250px_minmax(0,1fr)]">
      <a
        href="#main"
        className="sr-only focus:not-sr-only focus:fixed focus:left-3 focus:top-3 focus:z-50 btn-primary"
        onClick={e => {
          e.preventDefault()
          document.getElementById('main')?.focus()
        }}
      >
        Skip to content
      </a>
      {navOpen && (
        <button
          type="button"
          aria-label="Close menu"
          className="fixed inset-0 z-30 bg-black/50 md:hidden"
          onClick={() => setNavOpen(false)}
        />
      )}
      <aside
        id="primary-nav"
        className={[
          'border-r border-border bg-bg-elev flex flex-col min-h-0',
          'fixed inset-y-0 left-0 z-40 w-[250px] transition-transform',
          'md:static md:z-auto md:translate-x-0 md:bg-bg-elev/50 md:backdrop-blur',
          navOpen ? 'translate-x-0' : '-translate-x-full',
        ].join(' ')}
      >
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

        <nav className="px-3 pb-4 flex-1">
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
                {/* User management is the IdP's job, not a Nova screen: Keycloak owns
                    the realm and its console does it properly. The backend computes the
                    URL from the configured issuer; absent (dev mode, non-Keycloak issuer)
                    means no link, and non-admins never see this group. */}
                {group.admin && admin && me.data?.admin_console_url && (
                  <a
                    href={me.data.admin_console_url}
                    target="_blank"
                    rel="noreferrer"
                    title="Manage users in the platform identity provider"
                    className={navClass({ isActive: false })}
                  >
                    <Icon name="users" className="w-4 h-4" />
                    Users
                  </a>
                )}
              </div>
            </div>
            ))}
        </nav>



        {/* The project names itself: the version that is running, and where to read more
            or report a problem. A person should not have to leave Nova to find them. */}
        <footer className="px-3 pb-3 pt-2 border-t border-border">
          <div className="px-2 text-[11px] text-text-mute">stardelt v{__APP_VERSION__}</div>
          <div className="px-2 mt-1 flex flex-wrap gap-x-3 gap-y-1">
            <a
              href="https://stardelt.io"
              target="_blank"
              rel="noopener noreferrer"
              className="text-[11px] text-text-dim hover:text-text underline underline-offset-2 rounded-[4px] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
            >
              Documentation
            </a>
            <a
              href="https://github.com/stardelt/stardelt"
              target="_blank"
              rel="noopener noreferrer"
              className="text-[11px] text-text-dim hover:text-text underline underline-offset-2 rounded-[4px] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
            >
              Source
            </a>
          </div>
        </footer>
      </aside>

      <div className="flex flex-col min-w-0 min-h-0">
        <header className="h-14 shrink-0 border-b border-border bg-bg/30 backdrop-blur flex items-center gap-2 sm:gap-4 px-3 sm:px-6">
          <button
            type="button"
            className="btn-ghost px-2.5 py-2 md:hidden"
            aria-label="Open menu"
            aria-controls="primary-nav"
            aria-expanded={navOpen}
            onClick={() => setNavOpen(true)}
          >☰</button>
          <div className="text-sm font-medium truncate min-w-0 mr-auto">{title}</div>
          <form
            className="relative hidden sm:block"
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
              className="input pl-9 w-40 lg:w-64 py-1.5"
            />
          </form>
          <AppSwitcher />
          <UserMenu me={me.data} />
          <button
            onClick={toggleTheme}
            title={theme === 'dark' ? 'Switch to light' : 'Switch to dark'}
            className="btn-ghost px-2.5 py-2"
            aria-label="Toggle theme"
          >
            {theme === 'dark' ? <Icon name="spark" className="w-4 h-4" /> : <Icon name="pulse" className="w-4 h-4" />}
          </button>
        </header>

        <main id="main" tabIndex={-1} className="flex-1 min-h-0 overflow-auto p-3 sm:p-6 outline-none">
          <ErrorBoundary key={location.pathname}>
            <Outlet />
          </ErrorBoundary>
        </main>
      </div>
    </div>
  )
}
