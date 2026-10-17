import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api } from '../api'

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
  const apps = useQuery({ queryKey: ['apps'], queryFn: api.apps, refetchInterval: 15_000 })
  const superset = apps.data?.find(a => a.id === 'superset')
  const deployed = superset?.status === 'up'
  const base = superset?.external_url || FALLBACK

  const [path, setPath] = useState('/superset/welcome/')
  const src = `${base}${path}`

  return (
    <div className="h-full flex flex-col gap-4">
      <header className="flex items-center justify-between gap-4 flex-wrap">
        <div>
          <h1 className="text-3xl font-bold tracking-tight">Dashboards</h1>
          <p className="text-text-dim text-sm mt-1">
            Apache Superset, embedded.
            {deployed
              ? <> Login: <span className="font-mono">admin / admin</span>.</>
              : <> Not deployed yet — run <span className="font-mono">make install</span> (full profile) to add it.</>}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <span className={['badge', deployed ? 'border-success/40 bg-success/10 text-success' : 'border-border bg-bg-soft text-text-mute'].join(' ')}>
            <span className={['w-1.5 h-1.5 rounded-full', deployed ? 'bg-success' : 'bg-text-mute'].join(' ')} />
            {deployed ? 'running' : (superset?.status ?? 'unknown')}
          </span>
          <a href={src} target="_blank" rel="noreferrer" className="btn-ghost py-1.5">Open in new tab ↗</a>
        </div>
      </header>

      <div className="flex items-center gap-2 flex-wrap">
        {ENTRY_PATHS.map(e => (
          <button
            key={e.path}
            onClick={() => setPath(e.path)}
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
        <span className="ml-auto text-xs text-text-mute font-mono">{base}</span>
      </div>

      {deployed ? (
        <iframe
          title="Superset"
          src={src}
          className="flex-1 w-full rounded-[14px] border border-border bg-white"
        />
      ) : (
        <div className="card flex-1 grid place-items-center text-center p-10">
          <div>
            <div className="text-text-dim">Superset is not part of the core profile.</div>
            <div className="text-text-mute text-sm mt-1">
              It needs a bigger node; enable it with the <span className="font-mono">full</span> profile.
            </div>
            <a href={base} className="btn-ghost mt-4">Try {base} anyway</a>
          </div>
        </div>
      )}
    </div>
  )
}
