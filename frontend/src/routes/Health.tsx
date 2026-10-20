import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api, errorMessage } from '../api'
import { ClusterNotice } from '../components/ClusterNotice'
import ErrorBox from '../components/ErrorBox'
import { Icon } from '../components/Icon'
import { isBrowsable, safeHttpUrl, shortImage } from '../format'

export default function Health() {
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
          <h1 className="text-2xl font-bold tracking-tight">Platform health</h1>
          <p className="text-text-dim text-sm mt-1">Live probes for every service, from the backend.</p>
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
        <div className="card divide-y divide-border/70">
          {apps.isLoading && <div className="p-5 text-text-mute text-sm">loading…</div>}
          {apps.isError && !apps.data && <div className="p-5"><ErrorBox error={apps.error} onRetry={() => apps.refetch()} /></div>}
          {list.map(a => (
            <div key={a.id} className="flex items-center gap-3 px-5 py-3">
              <span className={['w-2 h-2 rounded-full', a.status === 'up' ? 'bg-success' : 'bg-text-mute'].join(' ')} />
              <Link to={`/apps/${a.id}`} className="font-medium hover:text-accent transition">{a.name}</Link>
              <span className="text-xs text-text-mute hidden sm:inline">{a.description}</span>
              <span className="ml-auto text-xs text-text-dim">{a.status === 'not-installed' ? 'not installed' : a.status}</span>
              {isBrowsable(a) && safeHttpUrl(a.external_url)
                ? <a href={safeHttpUrl(a.external_url)} target="_blank" rel="noreferrer" className="btn-ghost py-1 px-2.5 text-xs">
                    Open <Icon name="arrow" className="w-3.5 h-3.5" />
                  </a>
                : <span className="w-[68px]" />}
            </div>
          ))}
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
          <h2 className="text-sm font-semibold">Trino cluster</h2>
          <Link to="/apps" className="text-xs text-text-dim hover:text-text">All apps →</Link>
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
