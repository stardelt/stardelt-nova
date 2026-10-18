import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api, allTables, formatTime } from '../api'
import { Icon } from '../components/Icon'
import { isAdmin } from '../roles'
import { rowsLabel } from '../format'

export default function Overview() {
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const tables = useQuery({ queryKey: ['catalog', 'all'], queryFn: allTables })
  const jobs = useQuery({ queryKey: ['jobs'], queryFn: api.jobs, refetchInterval: 10_000 })
  // The engine and app cards are operator views: their endpoints answer 403 to everyone else,
  // so they are not fetched at all for a non-admin (an `enabled: false` query stays pending —
  // it must never be rendered as "loading…" or as an error).
  const admin = me.data ? isAdmin(me.data) : false
  const cluster = useQuery({ queryKey: ['trino', 'cluster'], queryFn: api.trinoCluster, retry: false, enabled: admin })
  const apps = useQuery({ queryKey: ['apps'], queryFn: api.apps, refetchInterval: 15_000, enabled: admin })

  const refs = tables.data ?? []
  const schemas = new Set(refs.map(r => r.ns)).size
  const info = cluster.data as Record<string, unknown> | undefined
  const engineState = typeof info?.state === 'string' ? info.state : cluster.isError ? 'unreachable' : '…'
  const recent = (jobs.data ?? []).slice(0, 6)

  return (
    <div className="space-y-6 max-w-6xl">
      <header className="flex items-end justify-between gap-4 flex-wrap">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Lakehouse overview</h1>
          <p className="text-text-dim text-sm mt-1">
            One catalog, one engine, one place to load and move data.
            {me.data?.demo && <span className="text-amber-300"> Demo data is in memory.</span>}
          </p>
        </div>
        <div className="flex gap-2">
          <Link to="/data" className="btn-primary"><Icon name="upload" />Load data</Link>
          <Link to="/query" className="btn-ghost"><Icon name="terminal" />New query</Link>
        </div>
      </header>

      <section className="grid grid-cols-2 lg:grid-cols-4 gap-4">
        <Stat label="Tables" value={tables.data ? String(refs.length) : tables.isError ? '—' : '…'} hint={tables.data || !tables.isError ? `${schemas} schemas` : 'catalog unavailable'} icon="table" />
        {admin && (
          <Stat label="Engine" value={String(engineState)} hint="Trino" icon="spark" tone={engineState === 'ACTIVE' ? 'ok' : undefined} />
        )}
        <Stat label="Jobs" value={jobs.data ? String(jobs.data.length) : jobs.isError ? '—' : '…'} hint="recent activity" icon="clock" />
        <Stat label="Format" value="Iceberg" hint="open tables on S3" icon="database" />
      </section>

      <section className="grid lg:grid-cols-3 gap-4">
        <div className={`card p-5 ${admin ? 'lg:col-span-2' : 'lg:col-span-3'}`}>
          <div className="flex items-center justify-between mb-3">
            <h2 className="text-sm font-semibold">Recent activity</h2>
            <Link to="/jobs" className="text-xs text-text-dim hover:text-text">View all →</Link>
          </div>
          {jobs.isLoading && <div className="text-text-mute text-sm">loading…</div>}
          {jobs.isError && !jobs.data && <div className="text-red-400 text-sm">{String(jobs.error)}</div>}
          {jobs.data && recent.length === 0 && (
            <div className="text-text-mute text-sm">No activity yet. Run a query or load a file.</div>
          )}
          <div className="space-y-1">
            {recent.map(j => (
              <div key={j.id} className="flex items-center gap-3 py-2 border-b border-border/60 last:border-0">
                <span className={kindBadge(j.kind)}>{j.kind}</span>
                <span className="text-sm truncate flex-1">{j.title}</span>
                <span className="text-xs text-text-mute">{j.rows != null ? rowsLabel(j.rows) : ''}</span>
                <span className="text-xs text-text-mute w-36 text-right">{formatTime(j.started_at)}</span>
              </div>
            ))}
          </div>
        </div>

        {admin && (
        <div className="card p-5">
          <div className="flex items-center justify-between mb-3">
            <h2 className="text-sm font-semibold">Platform apps</h2>
            <Link to="/apps" className="text-xs text-text-dim hover:text-text">All apps →</Link>
          </div>
          <div className="space-y-2 text-sm">
            {apps.isLoading && <div className="text-text-mute text-sm">loading…</div>}
            {apps.isError && !apps.data && <div className="text-red-400 text-sm">{String(apps.error)}</div>}
            {(apps.data ?? []).map(a => (
              <div key={a.id} className="flex items-center justify-between gap-3">
                <span className="flex items-center gap-2">
                  <span className={['w-1.5 h-1.5 rounded-full', a.status === 'up' ? 'bg-success' : 'bg-text-mute'].join(' ')} />
                  {a.name}
                </span>
                <span className="text-xs text-text-mute">{a.status === 'up' ? 'running' : a.status}</span>
              </div>
            ))}
          </div>
        </div>
        )}
      </section>

      <section className="grid md:grid-cols-3 gap-4">
        <Action to="/catalog" icon="database" title="Browse the catalog" body="Explore schemas, tables and column types." />
        <Action to="/data" icon="upload" title="Load & transfer" body="Ingest CSV or JSON, or copy a table." />
        {admin && (
          <Action to="/apps" icon="grid" title="Platform apps" body="Every service, live status and deep links." />
        )}
      </section>
    </div>
  )
}

function Stat({ label, value, hint, icon, tone }: {
  label: string
  value: string
  hint: string
  icon: 'table' | 'spark' | 'clock' | 'database'
  tone?: 'ok'
}) {
  return (
    <div className="card p-4">
      <div className="flex items-center justify-between">
        <span className="label">{label}</span>
        <Icon name={icon} className="w-4 h-4 text-text-mute" />
      </div>
      <div className={`text-2xl font-semibold mt-2 ${tone === 'ok' ? 'text-success' : ''}`}>{value}</div>
      <div className="text-xs text-text-mute mt-0.5">{hint}</div>
    </div>
  )
}

function Action({ to, icon, title, body }: { to: string; icon: 'database' | 'upload' | 'terminal' | 'grid'; title: string; body: string }) {
  return (
    <Link to={to} className="card p-5 hover:border-accent/40 transition group">
      <div className="flex items-center gap-2 text-sm font-semibold">
        <Icon name={icon} className="w-4 h-4 text-accent" />
        {title}
        <Icon name="arrow" className="w-4 h-4 ml-auto opacity-0 group-hover:opacity-100 transition" />
      </div>
      <p className="text-sm text-text-dim mt-2">{body}</p>
    </Link>
  )
}

function kindBadge(kind: string) {
  const map: Record<string, string> = {
    query: 'badge border-sky-400/40 bg-sky-400/10 text-sky-300',
    load: 'badge border-emerald-400/40 bg-emerald-400/10 text-emerald-300',
    transfer: 'badge border-violet-400/40 bg-violet-400/10 text-violet-300',
  }
  return map[kind] ?? 'badge border-border bg-bg-soft text-text-dim'
}
