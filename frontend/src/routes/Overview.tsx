import { useQuery } from '@tanstack/react-query'
import { useRef, useState, type ReactNode } from 'react'
import { Link } from 'react-router-dom'
import { ApiError, api, errorMessage, type CatalogStat } from '../api'
import { formatBytes, formatCount, formatTime, relativeTime, rowsLabel, statusLabel } from '../format'
import ErrorBox from '../components/ErrorBox'
import { Icon } from '../components/Icon'
import LayerBadge from '../components/LayerBadge'
import { isAdmin } from '../roles'

/** The personal setting that hides the first-run card once a person dismissed it. */
const FIRST_RUN_KEY = 'first-run-card-dismissed'

/** `1,234 rows`, and never the "1 rows" a plain template would print. */
function countRows(n: number): string {
  return `${formatCount(n)} ${n === 1 ? 'row' : 'rows'}`
}

/** When the newest numbers of the card were read, or `null` while nothing has been read. */
function newestUpdate(catalogs: CatalogStat[] | undefined): number | null {
  let newest: number | null = null
  for (const catalog of catalogs ?? []) {
    const at = catalog.updatedAt
    if (typeof at === 'number' && (newest === null || at > newest)) newest = at
  }
  return newest
}

export default function Overview() {
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  // The numbers are read in the background (#263), so the first answer is already there.
  // A catalog that has none yet is `pending`: re-ask every 5 s until it has numbers, and
  // stop after a minute — a scan that never finishes must not turn this page into a poller.
  const pendingSince = useRef<number | null>(null)
  const metrics = useQuery({
    queryKey: ['catalog', 'metrics'],
    queryFn: api.catalogMetrics,
    refetchInterval: (query) => {
      if (!query.state.data?.catalogs.some(catalog => catalog.pending)) {
        pendingSince.current = null
        return false
      }
      pendingSince.current ??= Date.now()
      return Date.now() - pendingSince.current < 60_000 ? 5_000 : false
    },
  })
  const jobs = useQuery({ queryKey: ['jobs'], queryFn: api.jobs, refetchInterval: 10_000 })
  // The engine and app cards are operator views: their endpoints answer 403 to everyone else,
  // so they are not fetched at all for a non-admin (an `enabled: false` query stays pending —
  // it must never be rendered as "loading…" or as an error).
  const admin = me.data ? isAdmin(me.data) : false
  const cluster = useQuery({ queryKey: ['trino', 'cluster'], queryFn: api.trinoCluster, retry: false, enabled: admin })
  const apps = useQuery({ queryKey: ['apps'], queryFn: api.apps, refetchInterval: 15_000, enabled: admin })

  // An administrator's Refresh starts a round in the background; the numbers follow with
  // the next read, which is why the page asks again five seconds later (a scan takes a
  // few). The button is disabled while anything is pending: no second round.
  const [refreshing, setRefreshing] = useState(false)
  const [refreshError, setRefreshError] = useState('')
  const pending = metrics.data?.catalogs.some(catalog => catalog.pending) ?? false
  async function refreshNow() {
    setRefreshing(true)
    setRefreshError('')
    try {
      await api.refreshCatalogMetrics()
      window.setTimeout(() => { void metrics.refetch() }, 5_000)
    } catch (e) {
      setRefreshError(errorMessage(e))
    } finally {
      setRefreshing(false)
    }
  }

  const totals = metrics.data?.totals
  // An instance with nothing to browse yet gets a card with the commands that publish a first
  // pipeline from Git. It waits for the numbers (a catalog still being counted reads zero) and
  // for the settings answer (no flash for someone who dismissed it), and it goes away as soon
  // as a table exists. An unreadable setting only means the card shows.
  const settings = useQuery({ queryKey: ['settings'], queryFn: api.settings, retry: false })
  const [dismissed, setDismissed] = useState(false)
  const firstRun =
    totals !== undefined && totals.tables === 0 && !pending &&
    (settings.isSuccess || settings.isError) &&
    !dismissed && settings.data?.[FIRST_RUN_KEY] !== true
  function dismissFirstRun() {
    setDismissed(true)
    void api.putSetting(FIRST_RUN_KEY, true).catch(() => {})
  }
  // A 403 is the API saying no catalog is open to this person: an answer, not a failure
  // they can retry away by clicking something.
  const denied = metrics.isError && metrics.error instanceof ApiError && metrics.error.status === 403
  const metricsFailure = metrics.isError ? errorMessage(metrics.error) : ''
  const info = cluster.data as Record<string, unknown> | undefined
  const engineState = typeof info?.state === 'string' ? info.state : cluster.isError ? 'unreachable' : '…'
  const recent = (jobs.data ?? []).slice(0, 6)

  return (
    <div className="space-y-6 max-w-6xl">
      <header className="flex items-end justify-between gap-4 flex-wrap">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Lakehouse overview</h1>
          <p className="text-text-dim text-sm mt-1">
            Your catalogs, one engine, one place to load and move data.
            {me.data?.demo && <span className="text-warn"> Demo data is in memory.</span>}
          </p>
        </div>
        <div className="flex gap-2">
          <Link to="/data" className="btn-primary"><Icon name="upload" />Load data</Link>
          <Link to="/query" className="btn-ghost"><Icon name="terminal" />New query</Link>
        </div>
      </header>

      {firstRun && <FirstRunCard group={me.data?.groups?.[0] ?? 'your-group'} onDismiss={dismissFirstRun} />}

      <section className="grid grid-cols-2 lg:grid-cols-4 gap-4">
        <Stat
          label="Catalogs"
          value={totals ? formatCount(totals.catalogs) : metrics.isError ? '—' : '…'}
          hint={totals ? `${formatCount(totals.schemas)} schemas` : metrics.isError ? metricsFailure : 'catalogs you can see'}
          icon="database"
        />
        <Stat
          label="Tables"
          value={totals ? formatCount(totals.tables) : metrics.isError ? '—' : '…'}
          hint={totals ? `${formatBytes(totals.sizeBytes)} · ${countRows(totals.records)}` : metrics.isError ? metricsFailure : 'across your catalogs'}
          icon="table"
        />
        {admin && (
          <Stat label="Engine" value={String(engineState)} hint="Trino" icon="spark" tone={engineState === 'ACTIVE' ? 'ok' : undefined} />
        )}
        <Stat label="Activity" value={jobs.data ? String(jobs.data.length) : jobs.isError ? '—' : '…'} hint="recent queries, loads and transfers" icon="clock" />
      </section>

      <CatalogsCard
        metrics={metrics.data?.catalogs}
        error={metrics.error}
        denied={denied}
        admin={admin}
        pending={pending}
        refreshing={refreshing}
        refreshError={refreshError}
        onRefresh={refreshNow}
      />

      <section className="grid lg:grid-cols-3 gap-4">
        <div className={`card p-5 ${admin ? 'lg:col-span-2' : 'lg:col-span-3'}`}>
          <div className="flex items-center justify-between mb-3">
            <h2 className="text-sm font-semibold">Recent activity</h2>
            <Link to="/jobs" className="text-xs text-text-dim hover:text-text">View all →</Link>
          </div>
          {jobs.isLoading && <div className="text-text-mute text-sm">loading…</div>}
          {jobs.isError && !jobs.data && <ErrorBox error={jobs.error} onRetry={() => jobs.refetch()} />}
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
            {apps.isError && !apps.data && <ErrorBox error={apps.error} onRetry={() => apps.refetch()} />}
            {(apps.data ?? []).map(a => (
              <div key={a.id} className="flex items-center justify-between gap-3">
                <span className="flex items-center gap-2">
                  <span className={['w-1.5 h-1.5 rounded-full', a.status === 'up' ? 'bg-success' : 'bg-text-mute'].join(' ')} />
                  {a.name}
                </span>
                <span className="text-xs text-text-mute">{statusLabel(a.status)}</span>
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

/**
 * The catalogs the person may see, one table row each (stardelt-nova#250). Every number is
 * the API's: a catalog whose scan failed keeps its row with the sentence, and a shortened
 * scan says so under the table instead of pretending the sample is the whole catalog.
 *
 * The numbers are read in the background (#263), so the card says how fresh they are and
 * an administrator can ask for a round now; a catalog with no numbers yet says
 * "calculating…" instead of zero, which would be a wrong number rather than none.
 */
function CatalogsCard({ metrics, error, denied, admin, pending, refreshing, refreshError, onRefresh }: {
  metrics: CatalogStat[] | undefined
  error: unknown
  denied: boolean
  admin: boolean
  pending: boolean
  refreshing: boolean
  refreshError: string
  onRefresh: () => void
}) {
  const updated = newestUpdate(metrics)
  return (
    <section className="card p-5 min-w-0">
      <div className="flex items-center justify-between gap-3 mb-3 flex-wrap">
        <div className="flex items-baseline gap-3 flex-wrap">
          <h2 className="text-sm font-semibold">Catalogs</h2>
          {updated !== null && (
            <span className="text-xs text-text-mute" title={formatTime(updated)}>
              Updated {relativeTime(updated)}
            </span>
          )}
        </div>
        <div className="flex items-center gap-3">
          {admin && (
            <button type="button" className="btn-ghost" onClick={onRefresh} disabled={pending || refreshing}>
              {refreshing ? 'Refreshing…' : 'Refresh'}
            </button>
          )}
          <Link to="/catalog" className="text-xs text-text-dim hover:text-text">Browse all →</Link>
        </div>
      </div>
      {refreshError && <p role="status" className="text-xs text-bad mb-2 break-words">{refreshError}</p>}
      {!metrics && !error && <div className="text-text-mute text-sm">loading…</div>}
      {denied && (
        <div className="space-y-1">
          <p role="status" className="text-sm break-words">{errorMessage(error)}</p>
          <p className="text-xs text-text-mute">Ask an administrator for access.</p>
        </div>
      )}
      {error != null && !denied && <ErrorBox error={error} />}
      {metrics && metrics.length === 0 && !error && (
        <p className="text-text-mute text-sm">No catalogs to show.</p>
      )}
      {metrics && metrics.length > 0 && (
        <>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <caption className="sr-only">Catalogs you can see</caption>
              <thead className="bg-bg-soft/50">
                <tr>
                  <th scope="col" className="th px-3 py-2.5">Catalog</th>
                  <th scope="col" className="th px-3 py-2.5 text-right">Schemas</th>
                  <th scope="col" className="th px-3 py-2.5 text-right">Tables</th>
                  <th scope="col" className="th px-3 py-2.5 text-right">Size</th>
                  <th scope="col" className="th px-3 py-2.5 text-right">Rows</th>
                  <th scope="col" className="th px-3 py-2.5 text-right">Last change</th>
                </tr>
              </thead>
              <tbody>
                {metrics.map(catalog => {
                  const commit = catalog.lastCommitMs
                  const changed = typeof commit === 'number' && Number.isFinite(commit) ? commit : null
                  return (
                    <tr key={catalog.name} className="border-t border-border/70">
                      <td className="td px-3">
                        <div className="flex flex-wrap items-center gap-2">
                          <Link
                            to={`/catalog?catalog=${encodeURIComponent(catalog.name)}`}
                            className="text-accent hover:underline break-words"
                          >
                            {catalog.name}
                          </Link>
                          <LayerBadge layer={catalog.layer} />
                        </div>
                        {catalog.error && (
                          <div role="status" className="text-xs text-bad mt-0.5 break-words">{catalog.error}</div>
                        )}
                      </td>
                      <td className="td px-3 text-right tabular-nums"><CellNumber pending={catalog.pending}>{formatCount(catalog.schemas)}</CellNumber></td>
                      <td className="td px-3 text-right tabular-nums"><CellNumber pending={catalog.pending}>{formatCount(catalog.tables)}</CellNumber></td>
                      <td className="td px-3 text-right tabular-nums whitespace-nowrap"><CellNumber pending={catalog.pending}>{formatBytes(catalog.sizeBytes)}</CellNumber></td>
                      <td className="td px-3 text-right tabular-nums"><CellNumber pending={catalog.pending}>{formatCount(catalog.records)}</CellNumber></td>
                      <td
                        className="td px-3 text-right whitespace-nowrap text-text-dim"
                        title={changed !== null ? formatTime(changed) : undefined}
                      >
                        <CellNumber pending={catalog.pending}>{changed !== null ? relativeTime(changed) : '—'}</CellNumber>
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
          {metrics.some(catalog => catalog.truncated) && (
            <p className="text-xs text-text-mute mt-2">first 200 tables counted</p>
          )}
        </>
      )}
    </section>
  )
}

/**
 * One number cell: the value, or "calculating…" while the catalog has no numbers yet. Muted
 * text, never a zero — a zero is a claim, and nothing has been read.
 */
function CellNumber({ pending, children }: { pending: boolean; children: ReactNode }) {
  return pending ? <span className="text-text-mute">calculating…</span> : <>{children}</>
}

/**
 * The first-run card: the commands that publish a first pipeline from Git, with this
 * instance's address and the person's own first group filled in. Text only — nothing here
 * runs anything, and nothing is written but the one personal "dismissed" setting.
 */
function FirstRunCard({ group, onDismiss }: { group: string; onDismiss: () => void }) {
  const commands = [
    'pip install stardelt',
    `export STARDELT_NOVA_URL=${window.location.origin}`,
    'stardelt login',
    `stardelt bundle init shop --name shop-etl --group ${group}`,
    'stardelt bundle deploy shop -t dev',
  ].join('\n')
  return (
    <section className="card p-5" aria-labelledby="first-run-title">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <h2 id="first-run-title" className="text-sm font-semibold">Publish your first pipeline from Git</h2>
          <p className="text-sm text-text-dim mt-1">
            This instance has no tables yet. A team keeps its pipelines in Git and publishes them with one command;
            the same commands run on your laptop and in CI.
          </p>
        </div>
        <button type="button" className="btn-ghost shrink-0" onClick={onDismiss}>Dismiss</button>
      </div>
      <pre className="mt-3 min-w-0 whitespace-pre-wrap wrap-anywhere rounded-[8px] border border-border/70 bg-bg-soft/50 p-3 font-mono text-[11px] select-all">{commands}</pre>
      <p className="text-xs text-text-mute mt-2">
        If the last command says your group has no pipeline identity yet, ask an administrator.{' '}
        <a
          href="https://stardelt.io/docs/guides/publish-pipelines"
          target="_blank"
          rel="noopener noreferrer"
          className="underline underline-offset-2 hover:text-text"
        >
          Read the guide
        </a>
      </p>
    </section>
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
      <div className="text-xs text-text-mute mt-0.5 break-words">{hint}</div>
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
    query: 'badge border-sky-400/40 bg-sky-400/10 text-info',
    load: 'badge border-emerald-400/40 bg-emerald-400/10 text-ok',
    transfer: 'badge border-violet-400/40 bg-violet-400/10 text-xfer',
  }
  return map[kind] ?? 'badge border-border bg-bg-soft text-text-dim'
}
