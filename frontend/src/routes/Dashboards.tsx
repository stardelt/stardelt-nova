import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api } from '../api'
import { isAdmin } from '../roles'
import { EmbedBadge, EmbedNotice } from '../components/EmbedNotice'
import { safeHttpUrl, statusLabel } from '../format'

// Superset runs as its own app. Prefer the deployed app URL (from /api/apps,
// which derives it from NOVA_BASE_DOMAIN); fall back to a port-forward.
const FALLBACK = (import.meta.env.VITE_SUPERSET_URL as string | undefined) ?? 'http://localhost:8089'

const ENTRY_PATHS: Array<{ label: string; path: string }> = [
  { label: 'Welcome',    path: '/superset/welcome/' },
  { label: 'SQL Lab',    path: '/sqllab/'           },
  { label: 'Dashboards', path: '/dashboard/list/'   },
  { label: 'Charts',     path: '/chart/list/'       },
  { label: 'Datasets',   path: '/tablemodelview/list/' },
  { label: 'Databases',  path: '/databaseview/list/'   },
]

export default function Dashboards() {
  // `/api/embeds` is readable by every signed-in user; `/api/apps` and its detail are
  // operator views (they carry workloads and images), so this page cannot depend on them.
  const apps = useQuery({ queryKey: ['embeds'], queryFn: api.embeds, refetchInterval: 15_000 })
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const admin = isAdmin(me.data)
  const superset = apps.data?.find(a => a.id === 'superset')
  const deployed = superset?.status === 'up'
  // Only http(s) URLs may reach `href` / iframe `src`; anything else falls back, then to nothing.
  const base = safeHttpUrl(superset?.external_url) ?? safeHttpUrl(FALLBACK)


  const [path, setPath] = useState('/superset/welcome/')
  // Only address the app when it is actually up: the fallback is a developer's
  // port-forward, and the deployed host answers 503 when nothing is behind the ingress.
  const src = deployed && base ? `${base.replace(/\/+$/, '')}${path}` : undefined

  return (
    <div className="h-full flex flex-col gap-4">
      <header className="flex items-center justify-between gap-4 flex-wrap">
        <div>
          <h1 className="text-3xl font-bold tracking-tight">Dashboards</h1>
          <p className="text-text-dim text-sm mt-1">
            Apache Superset, embedded.
            {deployed
              // The password is generated per cluster (scripts/app-secrets.sh in the
              // platform repo) and kept in the superset-admin Secret; the old
              // "admin / admin" hint stopped being true when that landed, and
              // printing a kubectl command here would hand a data engineer
              // Kubernetes vocabulary they do not need.
              ? <> Sign in with the Superset admin account.</>
              : apps.isLoading
                ? <> Checking whether it is deployed…</>
                : apps.isError
                  ? <> Could not read the app list ({String(apps.error)}).</>
                  : admin
                    ? <> Not deployed yet — run <span className="font-mono">make install</span> (full profile) to add it.</>
                    : <> Dashboards are not available yet. Ask your platform administrator to enable them.</>}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <EmbedBadge embed={superset?.embed} />
          <span className={['badge', deployed ? 'border-success/40 bg-success/10 text-success' : 'border-border bg-bg-soft text-text-mute'].join(' ')}>
            <span className={['w-1.5 h-1.5 rounded-full', deployed ? 'bg-success' : 'bg-text-mute'].join(' ')} />
            {statusLabel(superset?.status ?? 'unknown')}
          </span>
          {src && <a href={src} target="_blank" rel="noreferrer" className="btn-ghost py-1.5">Open in new tab ↗</a>}
        </div>
      </header>

      <EmbedNotice app="Superset" embed={superset?.embed} />

      <div className="flex items-center gap-2 flex-wrap">
        {ENTRY_PATHS.map(e => (
          <button
            key={e.path}
            onClick={() => setPath(e.path)}
            aria-pressed={path === e.path}
            className={[
              'px-3 py-1 rounded-[10px] text-sm border',
              path === e.path
                ? 'bg-accent/15 border-accent/30 text-text'
                : 'bg-bg-elev/40 border-border text-text-dim hover:text-text',
            ].join(' ')}
          >
            {e.label}
          </button>
        ))}
        <span className="ml-auto text-xs text-text-mute font-mono">{base ?? 'no valid Superset URL'}</span>
      </div>

      {deployed && src ? (
        // Superset needs scripts, its own cookies (allow-same-origin keeps it on *its* origin, which
        // is a separate subdomain from Nova), forms, popups (docs / permalinks) and downloads. It gets
        // no top-navigation, so the embedded page cannot redirect Nova itself.
        <iframe
          title="Superset"
          src={src}
          sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-popups-to-escape-sandbox allow-modals allow-downloads"
          allow="fullscreen; clipboard-write"
          className="flex-1 w-full rounded-[14px] border border-border bg-white"
        />
      ) : (
        <div className="card flex-1 grid place-items-center text-center p-10">
          <div>
            <div className="text-text-dim">
              {apps.isLoading ? 'Checking Superset…' : 'Superset is not part of the core profile.'}
            </div>
            <div className="text-text-mute text-sm mt-1">
              It needs a bigger node; enable it with the <span className="font-mono">full</span> profile.
            </div>
            {deployed && base && <a href={base} className="btn-ghost mt-4">Open {base}</a>}
          </div>
        </div>
      )}
    </div>
  )
}
