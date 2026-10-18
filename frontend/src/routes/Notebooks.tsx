import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api } from '../api'
import { EmbedBadge, EmbedNotice } from '../components/EmbedNotice'
import { safeHttpUrl } from '../format'

// JupyterHub runs as its own app. Prefer the deployed app URL (from /api/apps,
// which derives it from NOVA_BASE_DOMAIN); fall back to a port-forward
// (`kubectl -n stardelt port-forward svc/proxy-public 8000:80`).
const FALLBACK = (import.meta.env.VITE_JUPYTERHUB_URL as string | undefined) ?? 'http://localhost:8000'

// Hub-level paths only. JupyterLab does not live at `/lab` on the hub host: each
// person's server has its own URL (`/user/<name>/lab`), which is why a fixed
// "JupyterLab" entry cannot work — and why `/hub/home` is the right entry point,
// since the hub redirects it to that person's running server.
const ENTRY_PATHS: Array<{ label: string; path: string }> = [
  { label: 'Hub home',  path: '/hub/home'    },
  { label: 'My server', path: '/hub/spawn'   },
  { label: 'API tokens', path: '/hub/token'  },
]

export default function Notebooks() {
  const apps = useQuery({ queryKey: ['embeds'], queryFn: api.embeds, refetchInterval: 15_000 })
  const jupyter = apps.data?.find(a => a.id === 'jupyterhub')
  const up = jupyter?.status === 'up'
  // Only http(s) URLs may reach `href` / iframe `src`; anything else falls back, then to nothing.
  const base = safeHttpUrl(jupyter?.external_url) ?? safeHttpUrl(FALLBACK)


  const [path, setPath] = useState('/hub/home')
  // Only address the app when it is actually up: the fallback is a developer's
  // port-forward, and the deployed host answers 503 when nothing is behind the ingress.
  const src = up && base ? `${base.replace(/\/+$/, '')}${path}` : undefined

  return (
    <div className="h-full flex flex-col gap-4">
      <header className="flex items-center justify-between gap-4 flex-wrap">
        <div>
          <h1 className="text-3xl font-bold tracking-tight">Notebooks</h1>
          <p className="text-text-dim text-sm mt-1">
            JupyterHub, embedded — one notebook server per user, on the same catalog.
            {up
              ? <> Sign in with your platform account. If the frame below stays blank, open the hub once in a new tab (the identity provider does not allow being framed).</>
              : apps.isLoading
                ? <> Checking whether it is deployed…</>
                : apps.isError
                  ? <> Could not read the app list ({String(apps.error)}).</>
                  : <> Not deployed yet — add it with <span className="font-mono">make install-notebooks</span>.</>}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <EmbedBadge embed={jupyter?.embed} />
          <span className={['badge', up ? 'border-success/40 bg-success/10 text-success' : 'border-border bg-bg-soft text-text-mute'].join(' ')}>
            <span className={['w-1.5 h-1.5 rounded-full', up ? 'bg-success' : 'bg-text-mute'].join(' ')} />
            {up ? 'running' : (jupyter?.status ?? 'unknown')}
          </span>
          {src && <a href={src} target="_blank" rel="noreferrer" className="btn-ghost py-1.5">Open in new tab ↗</a>}
        </div>
      </header>

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
        <span className="ml-auto text-xs text-text-mute font-mono">{base ?? 'no valid JupyterHub URL'}</span>
      </div>

      <EmbedNotice app="JupyterHub" embed={jupyter?.embed} />

      {up && src ? (
        // Same trade-off as the Superset frame: scripts + its own origin's cookies, forms, popups,
        // modals and downloads, but no top-navigation. JupyterLab copies / pastes, hence clipboard.
        <iframe
          title="JupyterHub"
          src={src}
          sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-popups-to-escape-sandbox allow-modals allow-downloads"
          allow="fullscreen; clipboard-read; clipboard-write"
          className="flex-1 w-full rounded-[14px] border border-border bg-white"
        />
      ) : (
        <div className="card flex-1 grid place-items-center text-center p-10">
          <div>
            <div className="text-text-dim">
              {apps.isLoading ? 'Checking JupyterHub…' : 'JupyterHub is not installed in this platform.'}
            </div>
            <div className="text-text-mute text-sm mt-1 max-w-md">
              It fits on the dev node next to the core stack: it installs the hub, the
              configurable-http-proxy, a user scheduler and a placeholder StatefulSet — and it
              reaches the same Iceberg catalog every other workload does.
            </div>
            <div className="mt-4 rounded-[10px] border border-border bg-bg-soft/60 px-3 py-2 font-mono text-xs">
              make install-notebooks
            </div>
            {up && base && <a href={base} className="btn-ghost mt-4">Open {base}</a>}
          </div>
        </div>
      )}
    </div>
  )
}
