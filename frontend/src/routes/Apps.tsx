import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api, type PlatformApp } from '../api'
import { Icon } from '../components/Icon'
import { safeHttpUrl, shortImage } from '../format'

export default function Apps() {
  const apps = useQuery({ queryKey: ['apps'], queryFn: api.apps, refetchInterval: 15_000 })

  const groups = new Map<string, PlatformApp[]>()
  for (const app of apps.data ?? []) {
    const list = groups.get(app.category) ?? []
    list.push(app)
    groups.set(app.category, list)
  }

  const up = (apps.data ?? []).filter(a => a.status === 'up').length
  const total = apps.data?.length ?? 0

  return (
    <div className="max-w-6xl space-y-6">
      <header className="flex items-end justify-between gap-4 flex-wrap">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Platform apps</h1>
          <p className="text-text-dim text-sm mt-1">
            Every service in the platform, with live status and a direct link.
          </p>
        </div>
        <div className="text-sm text-text-dim">
          <span className="text-success font-semibold">{up}</span> / {total} up
        </div>
      </header>

      {apps.isLoading && <div className="text-text-mute">loading…</div>}
      {apps.isError && <div className="text-red-500 text-sm">{String(apps.error)}</div>}

      {[...groups.entries()].map(([category, list]) => (
        <section key={category}>
          <div className="label mb-2">{category}</div>
          <div className="grid sm:grid-cols-2 lg:grid-cols-3 gap-4">
            {list.map(app => <AppCard key={app.id} app={app} />)}
          </div>
        </section>
      ))}

      <p className="text-xs text-text-mute">
        Status is probed from the backend every 15s. Apps without a link are not exposed through the
        ingress (set <span className="font-mono">NOVA_BASE_DOMAIN</span> to enable links).
      </p>
    </div>
  )
}

function AppCard({ app }: { app: PlatformApp }) {
  const up = app.status === 'up'
  const externalUrl = safeHttpUrl(app.external_url)
  return (
    <div className="card p-5 flex flex-col gap-3 hover:border-accent/40 transition">
      <div className="flex items-start gap-3">
        <div className="w-9 h-9 rounded-[10px] bg-accent/15 border border-accent/25 grid place-items-center text-accent shrink-0">
          <Icon name="grid" className="w-4 h-4" />
        </div>
        <div className="min-w-0 flex-1">
          <Link to={`/apps/${app.id}`} className="font-semibold truncate block hover:text-accent transition">
            {app.name}
          </Link>
          <div className="text-xs text-text-dim leading-snug">{app.description}</div>
        </div>
      </div>

      {app.images.length > 0 && (
        <div className="space-y-1">
          {app.images.map(image => (
            <div key={image} className="text-[11px] font-mono text-text-mute truncate" title={image}>
              {shortImage(image)}
            </div>
          ))}
        </div>
      )}

      <div className="flex items-center gap-2 mt-auto">
        <span className={[
          'badge',
          up ? 'border-success/40 bg-success/10 text-success' : 'border-border bg-bg-soft text-text-mute',
        ].join(' ')}>
          <span className={['w-1.5 h-1.5 rounded-full', up ? 'bg-success' : 'bg-text-mute'].join(' ')} />
          {up ? 'running' : app.status}
        </span>
        <Link to={`/apps/${app.id}`} className="btn-ghost ml-auto py-1.5 px-3 text-xs">
          Details <Icon name="arrow" className="w-3.5 h-3.5" />
        </Link>
        {externalUrl && (
          <a
            href={externalUrl}
            target="_blank"
            rel="noreferrer"
            className="btn-ghost py-1.5 px-3 text-xs"
            title={`Open ${externalUrl}`}
          >
            Open
          </a>
        )}
      </div>
    </div>
  )
}
