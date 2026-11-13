import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api, errorMessage, type PlatformApp } from '../api'
import { ClusterNotice } from '../components/ClusterNotice'
import ErrorBox from '../components/ErrorBox'
import { Icon } from '../components/Icon'
import { isBrowsable, safeHttpUrl, shortImage, statusLabel } from '../format'

export default function Platform() {
  const apps = useQuery({ queryKey: ['apps'], queryFn: api.apps, refetchInterval: 10_000 })
  const cluster = useQuery({ queryKey: ['trino', 'cluster'], queryFn: api.trinoCluster, retry: false, refetchInterval: 15_000 })
  const workloads = useQuery({ queryKey: ['workloads'], queryFn: api.workloads, refetchInterval: 15_000 })
  const warehouse = useQuery({ queryKey: ['warehouse'], queryFn: api.warehouse })

  const list = apps.data ?? []
  // The "x / y services up" header counts installed services only: an app the cluster
  // holds no workload for was never installed, and counting it as down reads as an
  // outage (#70). Its row stays in the list below, labelled "not installed".
  const installed = list.filter(a => a.status !== 'not-installed')
  const up = installed.filter(a => a.status === 'up').length
  // One grid, problems first: an administrator opens this page to see what is broken.
  const rank: Record<PlatformApp['status'], number> = { unreachable: 0, unknown: 1, up: 2, 'not-installed': 3 }
  const sorted = [...list].sort((a, b) => rank[a.status] - rank[b.status] || a.name.localeCompare(b.name))
  // The snapshot, not a bare array: `rows` may be the last known state rather than the
  // current one, which is exactly what the notice below is for.
  const snapshot = workloads.data
  const rows = snapshot?.workloads ?? []
  const info = cluster.data as Record<string, unknown> | undefined
  const version = (info?.nodeVersion as Record<string, unknown> | undefined)?.version
  const state = typeof info?.state === 'string' ? info.state : cluster.isError ? 'unreachable' : '…'

  return (
    <div className="max-w-6xl space-y-6">
      <header className="flex items-end justify-between gap-4 flex-wrap">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Platform</h1>
          <p className="text-text-dim text-sm mt-1">Every service, its workloads and the query engine, as Nova sees them now.</p>
        </div>
        <div className="text-sm text-text-dim">
          <span className={up === installed.length && installed.length > 0 ? 'text-success font-semibold' : 'font-semibold'}>{up}</span>
          {' / '}{installed.length} services up
        </div>
      </header>

      <section className="grid grid-cols-2 lg:grid-cols-4 gap-4">
        <Stat label="Engine state" value={String(state)} hint="Trino" ok={state === 'ACTIVE'} />
        <Stat label="Engine version" value={version ? String(version) : '—'} hint="Trino" />
        <Stat label="Services" value={`${up}/${installed.length}`} hint="probing every 10s" ok={up === installed.length && installed.length > 0} />
        <Stat label="Warehouse" value={warehouse.data?.name ?? (warehouse.isError ? '—' : '…')} hint="Iceberg REST" />
      </section>

      <section>
        <div className="label mb-2">Services</div>
        {apps.isLoading && <div className="text-text-mute text-sm">loading…</div>}
        {apps.isError && !apps.data && <ErrorBox error={apps.error} onRetry={() => apps.refetch()} />}
        <div className="grid sm:grid-cols-2 lg:grid-cols-3 gap-4">
          {sorted.map(app => <AppCard key={app.id} app={app} />)}
        </div>
      </section>

      <section>
        <div className="label mb-2">Running images</div>
        <div className="space-y-2">
          <ClusterNotice snapshot={snapshot} />
          <div className="card divide-y divide-border/70">
            {workloads.isLoading && <div className="p-5 text-text-mute text-sm">loading…</div>}
            {workloads.isError && !workloads.data && <div className="p-5"><ErrorBox error={workloads.error} onRetry={() => workloads.refetch()} /></div>}
            {rows.map(w => (
              <div key={`${w.kind}/${w.name}`} className="flex items-center gap-3 px-5 py-2.5">
                <span className="w-16 shrink-0 text-[11px] uppercase tracking-wider text-text-mute">
                  {w.kind === 'StatefulSet' ? 'sts' : 'deploy'}
                </span>
                <span className="w-40 shrink-0 truncate text-sm font-medium">{w.name}</span>
                <span className="truncate text-[11px] font-mono text-text-dim" title={w.image}>
                  {shortImage(w.image)}
                </span>
                <span className={[
                  'ml-auto shrink-0 text-xs',
                  w.desired > 0 && w.ready === w.desired ? 'text-success' : 'text-text-mute',
                ].join(' ')}>
                  {w.ready}/{w.desired}
                </span>
              </div>
            ))}
            {!workloads.isLoading && rows.length === 0 && (
              <div className="p-5 text-sm text-text-mute">
                {snapshot?.state === 'ok'
                  ? "The cluster answered — no deployments or statefulsets in Nova's namespace."
                  : 'No cluster access — Nova lists images only when it can read its own namespace.'}
              </div>
            )}
          </div>
        </div>
        <p className="text-xs text-text-mute mt-2">
          Read live from the cluster, so a digest pin shows as the digest, not as the tag someone
          meant to deploy.
        </p>
      </section>

      <section className="card p-5">
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-sm font-semibold">Query engine</h2>
        </div>
        <pre className="text-xs font-mono text-text-dim overflow-auto max-h-64">
{JSON.stringify(cluster.data ?? { error: cluster.error ? errorMessage(cluster.error) : 'loading' }, null, 2)}
        </pre>
      </section>
    </div>
  )
}

function Stat({ label, value, hint, ok }: { label: string; value: string; hint: string; ok?: boolean }) {
  return (
    <div className="card p-4">
      <div className="text-[11px] uppercase tracking-widest text-text-mute">{label}</div>
      <div className={`text-2xl font-semibold mt-2 ${ok ? 'text-success' : ''}`}>{value}</div>
      <div className="text-xs text-text-mute mt-0.5">{hint}</div>
    </div>
  )
}

function AppCard({ app }: { app: PlatformApp }) {
  const up = app.status === 'up'
  const failing = app.status === 'unreachable' || app.status === 'unknown'
  const externalUrl = isBrowsable(app) ? safeHttpUrl(app.external_url) : undefined
  return (
    <div className="card p-5 flex flex-col gap-3 hover:border-accent/40 transition">
      <div className="flex items-start gap-3">
        <div className="w-9 h-9 rounded-[10px] bg-accent/15 border border-accent/25 grid place-items-center text-accent shrink-0">
          <Icon name="grid" className="w-4 h-4" />
        </div>
        <div className="min-w-0 flex-1">
          <div className="text-[11px] uppercase tracking-wider text-text-mute">{app.category}</div>
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
          up ? 'border-success/40 bg-success/10 text-success' : failing ? 'border-warn/40 bg-warn/10 text-warn' : 'border-border bg-bg-soft text-text-mute',
        ].join(' ')}>
          <span className={['w-1.5 h-1.5 rounded-full', up ? 'bg-success' : failing ? 'bg-warn' : 'bg-text-mute'].join(' ')} />
          {statusLabel(app.status)}
        </span>
        <Link to={`/apps/${app.id}`} className="btn-ghost ml-auto py-1.5 px-3 text-xs">
          App details <Icon name="arrow" className="w-3.5 h-3.5" />
        </Link>
        {/* Only link what is actually serving. The ingress exists for every app with a
            UI in the playground, so an uninstalled Superset answers with an ingress 503
            — offering that link is how a user ends up staring at one — and an app with
            no UI at all (Lakekeeper) has no ingress, hence no link. See issue #70 for
            the not-installed-vs-unreachable wording, #69 for `browsable`. */}
        {externalUrl && up && (
          <a
            href={externalUrl}
            target="_blank"
            rel="noreferrer"
            className="btn-ghost py-1.5 px-3 text-xs"
            title={`Open ${externalUrl}`}
          >
            Open app ↗
          </a>
        )}
        {externalUrl && !up && (
          <span className="py-1.5 px-3 text-xs text-text-mute" title={`${externalUrl} is not answering yet`}>
            no link
          </span>
        )}
      </div>
    </div>
  )
}
