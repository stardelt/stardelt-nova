import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api, errorMessage } from '../api'
import { isAdmin } from '../roles'
import PublishedNotebooks from '../components/PublishedNotebooks'
import { safeHttpUrl } from '../format'

// JupyterHub runs as its own app. Prefer the deployed app URL (from /api/apps,
// which derives it from NOVA_BASE_DOMAIN); fall back to a port-forward
// (`kubectl -n stardelt port-forward svc/proxy-public 8000:80`).
const FALLBACK = (import.meta.env.VITE_JUPYTERHUB_URL as string | undefined) ?? 'http://localhost:8000'

// Hub-level paths only. JupyterLab does not live at `/lab` on the hub host: each
// person's server has its own URL (`/user/<name>/lab`), which is why a fixed
// "JupyterLab" entry cannot work. `/hub/` is the hub *root* and the right default:
// it redirects a signed-in person to their own server (JupyterLab — or the spawn
// page when they have none), while `/hub/home` is the control panel and stays put.
const HUBS: Array<{ label: string; path: string }> = [
  { label: 'My server', path: '/hub/'      },
  { label: 'Start server', path: '/hub/spawn' },
  { label: 'API tokens', path: '/hub/token'  },
  { label: 'Hub home', path: '/hub/home'    },
]

/**
 * Notebooks: the published notebooks of the caller's groups, and the way into
 * JupyterHub. The embedded frame is gone on purpose (2026-11-02) — JupyterHub is
 * a first-class app in the launcher (`/apps`), and an app the browser frames badly
 * is worse than a button that opens it properly. What stays here is what this page
 * is for: the published notebooks, and the way into the hub. The VS Code steps,
 * the server and the API tokens are personal settings and live on Profile (Profile
 * card "Notebooks: your server, tokens and VS Code", stardelt-nova#516).
 */
export default function Notebooks() {
  const apps = useQuery({ queryKey: ['embeds'], queryFn: api.embeds, refetchInterval: 15_000 })
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const admin = isAdmin(me.data)
  const jupyter = apps.data?.find(a => a.id === 'jupyterhub')
  const up = jupyter?.status === 'up'
  // Only http(s) URLs may reach `href`; anything else falls back, then to nothing.
  const base = (safeHttpUrl(jupyter?.external_url) ?? safeHttpUrl(FALLBACK))?.replace(/\/+$/, '')
  const at = (path: string) => (up && base ? `${base}${path}` : undefined)
  const open = at('/hub/')

  return (
    <div className="space-y-5">
      <header className="flex flex-wrap items-start justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-2xl font-bold tracking-tight">Notebooks</h1>
          <p className="text-text-dim text-sm mt-1">
            Published notebooks from your groups. Write and run them in JupyterHub — same Keycloak
            account, same catalog.
          </p>
        </div>
        <div className="flex items-center gap-2">
          {open && (
            <a href={open} target="_blank" rel="noreferrer" className="btn-primary py-1.5">
              Open JupyterHub ↗
            </a>
          )}
          <Link to="/profile" className="btn-ghost py-1.5 text-xs">VS Code and tokens</Link>
        </div>
      </header>

      {up && base ? (
        <div className="flex flex-wrap items-center gap-2 text-sm">
          {HUBS.map(hub => (
            <a
              key={hub.path}
              href={at(hub.path)}
              target="_blank"
              rel="noreferrer"
              className="px-3 py-1 rounded-[10px] border border-border bg-bg-elev/40 text-text-dim hover:text-text"
            >
              {hub.label}
            </a>
          ))}
          <span className="ml-auto text-xs text-text-mute font-mono">{base}</span>
        </div>
      ) : (
        <div className="card px-5 py-4 text-sm text-text-dim">
          {apps.isLoading
            ? 'Checking whether JupyterHub is deployed…'
            : apps.isError
              ? <>Could not read the app list ({errorMessage(apps.error)}).</>
              : admin
                ? <>JupyterHub is not installed in this platform. Add it with <span className="font-mono">make install-notebooks</span>.</>
                : 'Notebooks are not available yet. Ask your platform administrator to enable them.'}
        </div>
      )}

      <PublishedNotebooks />
    </div>
  )
}
