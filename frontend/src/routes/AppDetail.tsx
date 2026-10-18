import { useQuery } from '@tanstack/react-query'
import { Link, useParams } from 'react-router-dom'
import { api, ApiError, type AppFact } from '../api'
import { Icon } from '../components/Icon'
import { safeHttpUrl, shortImage } from '../format'

/**
 * Nova's own view of an app's data — the part of "using" the service that Nova
 * does itself, as opposed to the app's own UI behind an external link.
 */
const NOVA_ROUTES: Record<string, Array<{ to: string; label: string }>> = {
  lakekeeper: [
    { to: '/catalog', label: 'Browse the catalog' },
    { to: '/query', label: 'Query a table' },
  ],
  trino: [{ to: '/query', label: 'Run SQL' }],
  superset: [{ to: '/dashboards', label: 'Embedded Superset' }],
  jupyterhub: [{ to: '/notebooks', label: 'Embedded JupyterHub' }],
}

export default function AppDetail() {
  const { id = '' } = useParams()
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const detail = useQuery({
    queryKey: ['app', id],
    queryFn: () => api.app(id),
    refetchInterval: 15_000,
    retry: false,
  })

  // A failed 15s refresh keeps the last good `detail.data`; only replace the page when there is none.
  if (detail.isError && !detail.data) {
    const unknown = detail.error instanceof ApiError && detail.error.status === 404
    return (
      <div className="max-w-6xl space-y-4">
        <Link to="/apps" className="text-sm text-text-dim hover:text-text inline-flex items-center gap-1">
          ← All apps
        </Link>
        <div className="card p-6">
          {unknown ? (
            <>
              <div className="font-semibold">Unknown app “{id}”.</div>
              <p className="text-text-dim text-sm mt-1">
                Nova only knows the services it can wire into the platform. The list is on the{' '}
                <Link to="/apps" className="text-accent hover:underline">apps page</Link>.
              </p>
            </>
          ) : (
            <>
              <div className="font-semibold">Could not load “{id}”.</div>
              <p className="text-text-dim text-sm mt-1">
                The Nova backend did not answer with the app's details.{' '}
                <button onClick={() => detail.refetch()} className="text-accent hover:underline">Try again</button>
              </p>
            </>
          )}
          <div className="text-xs text-text-mute mt-3 font-mono">{String(detail.error)}</div>
        </div>
      </div>
    )
  }

  if (!detail.data) return <div className="text-text-mute">loading…</div>

  const { app, probe, replicas, workloads, facts, links } = detail.data
  const up = app.status === 'up'
  const novaRoutes = NOVA_ROUTES[app.id] ?? []
  // `browsable` is landing in the API separately (issue #69). Until it does, this page
  // already knows the answer: with a base domain, the detail response carries links
  // only for an app that has paths a browser can open.
  const browsable = app.browsable ?? (links.length > 0 || app.external_url === '')
  const externalUrl = browsable ? safeHttpUrl(app.external_url) : undefined

  return (
    <div className="max-w-6xl space-y-6">
      <Link to="/apps" className="text-sm text-text-dim hover:text-text inline-flex items-center gap-1">
        ← All apps
      </Link>

      <header className="flex items-start justify-between gap-4 flex-wrap">
        <div>
          <div className="flex items-center gap-2">
            <h1 className="text-2xl font-bold tracking-tight">{app.name}</h1>
            <span className="badge border-border bg-bg-soft text-text-dim">{app.category}</span>
          </div>
          <p className="text-text-dim text-sm mt-1">{app.description}</p>
        </div>
        <div className="flex items-center gap-2">
          <span className={[
            'badge',
            up ? 'border-success/40 bg-success/10 text-success' : 'border-border bg-bg-soft text-text-mute',
          ].join(' ')}>
            <span className={['w-1.5 h-1.5 rounded-full', up ? 'bg-success' : 'bg-text-mute'].join(' ')} />
            {up ? 'running' : app.status}
          </span>
          {workloads.length > 0 && (
            <span className="badge border-border bg-bg-soft text-text-dim" title="ready/desired replicas across this app's workloads">
              {replicas.ready}/{replicas.desired} replicas
            </span>
          )}
          {externalUrl ? (
            <a href={externalUrl} target="_blank" rel="noreferrer" className="btn-ghost py-1.5">
              Open <Icon name="arrow" className="w-3.5 h-3.5" />
            </a>
          ) : (
            <span className="text-xs text-text-mute">no external link</span>
          )}
        </div>
      </header>

      <section>
        <div className="label mb-2">What Nova can see</div>
        {facts.length > 0 ? (
          <div className="grid sm:grid-cols-2 lg:grid-cols-3 gap-4">
            {facts.map(f => <FactCard key={`${f.label}-${f.value}`} fact={f} />)}
          </div>
        ) : (
          <div className="card p-5 text-sm text-text-dim">
            No extra facts. Nova reads them from the service's own API — a service that is
            unreachable, or one that exposes no API Nova may call, contributes none.
            {me.data?.demo && <span className="text-text-mute"> Demo mode has no service APIs at all.</span>}
          </div>
        )}
      </section>

      <section>
        <div className="label mb-2">Workloads</div>
        <div className="card divide-y divide-border/70">
          {workloads.map(w => (
            <div key={`${w.kind}/${w.name}`} className="flex items-center gap-3 px-5 py-2.5">
              <span className="w-16 shrink-0 text-[11px] uppercase tracking-wider text-text-mute">
                {w.kind === 'StatefulSet' ? 'sts' : 'deploy'}
              </span>
              <span className="w-44 shrink-0 truncate text-sm font-medium">{w.name}</span>
              <span className="truncate text-[11px] font-mono text-text-dim" title={w.image}>
                {w.image ? shortImage(w.image) : '—'}
              </span>
              <span className={[
                'ml-auto shrink-0 text-xs',
                w.desired > 0 && w.ready === w.desired ? 'text-success' : 'text-text-mute',
              ].join(' ')}>
                {w.ready}/{w.desired}
              </span>
            </div>
          ))}
          {workloads.length === 0 && (
            <div className="p-5 text-sm text-text-mute">
              Nova sees no workloads for this service in its namespace. Either it is not installed
              here, or Nova has no cluster access — demo mode runs without one.
            </div>
          )}
        </div>
        <p className="text-xs text-text-mute mt-2">
          Deployments and StatefulSets read live from the cluster, with the image each one runs.
        </p>
      </section>

      <section className="grid lg:grid-cols-3 gap-4">
        <div className="card p-5 lg:col-span-2">
          <h2 className="text-sm font-semibold mb-3">{app.name} links</h2>
          <div className="flex flex-wrap gap-2">
            {links.map(l => {
              const href = safeHttpUrl(l.url)
              return href ? (
                <a key={l.url} href={href} target="_blank" rel="noreferrer" className="btn-ghost py-1.5 text-xs">
                  {l.label} <Icon name="arrow" className="w-3.5 h-3.5" />
                </a>
              ) : null
            })}
            {links.length === 0 && browsable && app.external_url === '' && (
              <span className="text-sm text-text-mute">
                No external URL: the platform install sets <span className="font-mono">NOVA_BASE_DOMAIN</span>,
                and Nova derives every app link from it.
              </span>
            )}
            {links.length === 0 && !browsable && (
              <span className="text-sm text-text-mute">
                {app.name} has no browsable UI of its own — Nova talks to its API directly.
              </span>
            )}
          </div>
          {novaRoutes.length > 0 && (
            <div className="mt-4 pt-4 border-t border-border/70">
              <div className="label mb-2">In Nova</div>
              <div className="flex flex-wrap gap-2">
                {novaRoutes.map(r => (
                  <Link key={r.to} to={r.to} className="btn-ghost py-1.5 text-xs">
                    {r.label} <Icon name="arrow" className="w-3.5 h-3.5" />
                  </Link>
                ))}
              </div>
            </div>
          )}
        </div>

        <div className="card p-5">
          <div className="label">Liveness</div>
          <div className="text-sm mt-2">
            The backend probes this URL every request; <span className="font-mono text-xs">{up ? '< 500' : 'no answer'}</span>{' '}
            is why the badge {up ? 'says running' : 'does not say running'}.
          </div>
          <div className="mt-3 rounded-[10px] border border-border bg-bg-soft/60 px-3 py-2 font-mono text-[11px] break-all">
            {probe}
          </div>
          <p className="text-xs text-text-mute mt-3">
            Probed from inside the cluster — a service without an external URL is still health-checked.
          </p>
        </div>
      </section>
    </div>
  )
}

function FactCard({ fact }: { fact: AppFact }) {
  const tone = fact.tone === 'ok' ? 'text-success' : fact.tone === 'bad' ? 'text-red-400' : ''
  return (
    <div className="card p-4">
      <div className="label">{fact.label}</div>
      <div className={`text-lg font-semibold mt-1 break-words ${tone}`}>{fact.value}</div>
      {fact.source && <div className="text-[11px] text-text-mute mt-1 font-mono">{fact.source}</div>}
    </div>
  )
}
