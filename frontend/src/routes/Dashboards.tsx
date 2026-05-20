import { useState } from 'react'

// Superset embed URL. The browser reaches Superset via its own port-forward
// (default localhost:8089 per `make pf`), not via Nova's backend. This is
// configurable so a future production setup can swap in a same-origin
// reverse-proxy path.
const DEFAULT_SUPERSET_URL =
  (import.meta.env.VITE_SUPERSET_URL as string | undefined) ?? 'http://localhost:8089'

const ENTRY_PATHS: Array<{ label: string; path: string }> = [
  { label: 'Welcome',     path: '/superset/welcome/' },
  { label: 'SQL Lab',     path: '/sqllab/'           },
  { label: 'Dashboards',  path: '/dashboard/list/'   },
  { label: 'Charts',      path: '/chart/list/'       },
  { label: 'Datasets',    path: '/tablemodelview/list/' },
  { label: 'Databases',   path: '/databaseview/list/'   },
]

export default function Dashboards() {
  const [base, setBase] = useState(DEFAULT_SUPERSET_URL)
  const [path, setPath] = useState('/superset/welcome/')
  const src = `${base}${path}`

  return (
    <div className="h-full flex flex-col gap-4">
      <header className="flex items-center justify-between gap-4 flex-wrap">
        <div>
          <h1 className="text-3xl font-bold tracking-tight">Dashboards</h1>
          <p className="text-text-dim text-sm mt-1">
            Apache Superset embedded. Login: <span className="font-mono">admin / admin</span>.
            Requires <span className="font-mono">make pf</span> running.
          </p>
        </div>
        <a
          href={src}
          target="_blank"
          rel="noreferrer"
          className="px-4 py-2 rounded-[10px] border border-border bg-bg-elev/60 hover:border-accent/50 text-sm"
        >
          Open in new tab ↗
        </a>
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
        <input
          type="text"
          value={base}
          onChange={e => setBase(e.target.value)}
          className="ml-auto bg-bg-elev/60 border border-border rounded-[10px] px-3 py-1 text-xs font-mono w-72"
          placeholder="Superset base URL"
        />
      </div>

      <iframe
        title="Superset"
        src={src}
        className="flex-1 w-full rounded-[14px] border border-border bg-white"
      />
    </div>
  )
}
