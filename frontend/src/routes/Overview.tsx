import { useQuery, type UseQueryResult } from '@tanstack/react-query'
import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { Link } from 'react-router-dom'
import { ApiError, api, errorMessage, type CatalogStat, type QueryLogItem, type RunsListResponse } from '../api'
import { mergeActivity, type ActivityRow } from '../activity'
import { coveredStart, dayStarts, oldestOf, queriesPerDay, runsPerDay, type RunBucket } from '../charts'
import { formatBytes, formatCount, formatTime, relativeTime, statusLabel } from '../format'
import ErrorBox from '../components/ErrorBox'
import { EmptyState } from '../components/EmptyState'
import FreshnessCard from '../components/FreshnessCard'
import { QuerySparkline, RunBars } from '../components/PulseCharts'
import { Skeleton, SkeletonTable } from '../components/Skeleton'
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

const PULSE_QUERY_PAGE_LIMIT = 200
const PULSE_QUERY_MAX_PAGES = 20 // 4,000 rows; the API caps a page at 200
const PULSE_RUNS_LIMIT = 200 // the API's `RUNS_MAX_LIMIT`; at this size the answer may be cut
async function loadPulseQueries(from: number) {
  const items: QueryLogItem[] = []
  let cursor: string | undefined
  for (let page = 0; page < PULSE_QUERY_MAX_PAGES; page++) {
    const answer = await api.queryLog({ scope: 'mine', from, limit: PULSE_QUERY_PAGE_LIMIT, cursor })
    items.push(...answer.items)
    if (!answer.nextCursor) return { items, truncated: false }
    cursor = answer.nextCursor
  }
  return { items, truncated: true }
}

/**
 * True once the element has come within 200 px of the viewport (#510 item 6). The timeline's
 * two list reads are started only then; `jobs` stays eager because the Activity stat above the
 * fold counts it.
 */
function useNearViewport<T extends Element>() {
  const ref = useRef<T>(null)
  const [near, setNear] = useState(false)
  useEffect(() => {
    const node = ref.current
    if (!node || near) return
    if (typeof IntersectionObserver === 'undefined') { setNear(true); return }
    const observer = new IntersectionObserver(
      (entries) => { if (entries.some((e) => e.isIntersecting)) { setNear(true); observer.disconnect() } },
      { rootMargin: '200px' },
    )
    observer.observe(node)
    return () => observer.disconnect()
  }, [near])
  return { ref, near }
}

export default function Overview() {
  // The page's own budget (#510 item 6): an answer younger than 30 s is not read again when
  // this page is mounted another time. `refetchInterval` still re-reads while it is open.
  const me = useQuery({ queryKey: ['me'], queryFn: api.me, staleTime: 30_000 })
  // The numbers are read in the background (#263), so the first answer is already there.
  // A catalog that has none yet is `pending`: re-ask every 5 s until it has numbers, and
  // stop after a minute — a scan that never finishes must not turn this page into a poller.
  const pendingSince = useRef<number | null>(null)
  const metrics = useQuery({
    queryKey: ['catalog', 'metrics'],
    queryFn: api.catalogMetrics,
    staleTime: 30_000,
    refetchInterval: (query) => {
      if (!query.state.data?.catalogs.some(catalog => catalog.pending)) {
        pendingSince.current = null
        return false
      }
      pendingSince.current ??= Date.now()
      return Date.now() - pendingSince.current < 60_000 ? 5_000 : false
    },
  })
  // The timeline is one list over three sources with different rules (#761): runs are the
  // runs of the caller's group-owned pipelines, queries follow the scope chosen here, and
  // loads are always the caller's own. The endpoints keep enforcing their own visibility.
  const [scope, setScope] = useState<'me' | 'groups'>('groups')
  // The Recent activity card sits below the fold, so its two reads wait until it is near.
  const timeline = useNearViewport<HTMLElement>()
  const runs = useQuery({ queryKey: ['runs'], queryFn: () => api.runs(), refetchInterval: 10_000, staleTime: 30_000, enabled: timeline.near })
  const queries = useQuery({
    queryKey: ['query-log', 'overview', scope],
    queryFn: () => api.queryLog({ scope: scope === 'me' ? 'mine' : 'team', limit: 20 }),
    refetchInterval: 10_000,
    staleTime: 30_000,
    enabled: timeline.near,
  })
  const jobs = useQuery({ queryKey: ['jobs'], queryFn: api.jobs, refetchInterval: 10_000, staleTime: 30_000 })
  // The two small charts (#767): the same list endpoints, one read per chart. The query log
  // is read page by page over the 14-day window (nova#883): one page of 200 rows drew false
  // zero days when the window held more. `pulseFrom` is the first midnight of the 14-day
  // chart, so the query log is asked only for the window the chart draws.
  const pulseFrom = useMemo(() => dayStarts(14, Date.now())[0], [])
  const pulseQueries = useQuery({
    queryKey: ['query-log', 'pulse'],
    queryFn: () => loadPulseQueries(pulseFrom),
    refetchInterval: 60_000,
    staleTime: 30_000,
  })
  const pulseRuns = useQuery({
    queryKey: ['runs', 'pulse'],
    queryFn: () => api.runs({ limit: PULSE_RUNS_LIMIT }),
    refetchInterval: 60_000,
    staleTime: 30_000,
  })
  // The engine and app cards are operator views: their endpoints answer 403 to everyone else,
  // so they are not fetched at all for a non-admin (an `enabled: false` query stays pending —
  // it must never be rendered as "loading…" or as an error).
  const admin = me.data ? isAdmin(me.data) : false
  const cluster = useQuery({ queryKey: ['trino', 'cluster'], queryFn: api.trinoCluster, retry: false, staleTime: 30_000, enabled: admin })
  const apps = useQuery({ queryKey: ['apps'], queryFn: api.apps, refetchInterval: 15_000, staleTime: 30_000, enabled: admin })

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
  const settings = useQuery({ queryKey: ['settings'], queryFn: api.settings, retry: false, staleTime: 30_000 })
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
  const recent = mergeActivity(scope === 'groups' ? runs.data?.runs : undefined, queries.data?.items, jobs.data)
  // One unreadable source must not blank the other two: the list shows what arrived, and the
  // failed source keeps its own line with its own retry underneath.
  const sources = [
    { key: 'runs' as const, label: 'runs', query: runs },
    { key: 'queries' as const, label: 'queries', query: queries },
    { key: 'loads' as const, label: 'loads', query: jobs },
  ]
  const allLoading = sources.every(source => source.query.isLoading)
  const allFailed = sources.every(source => source.query.isError)
  const failed = allFailed ? [] : sources.filter(source => source.query.isError)
  // The charts' windows (#767). The counts are derived here, once per render, and the card
  // only draws them.
  const now = Date.now()
  const queryDays = dayStarts(14, now)
  const runDays = dayStarts(7, now)
  const queryCounts = queriesPerDay((pulseQueries.data?.items ?? []).map(q => q.createdAt), 14, now)
  const runBuckets = runsPerDay(pulseRuns.data?.runs ?? [], 7, now)
  // A truncated answer is not a quiet week (#882): start each chart at the oldest day it covers.
  const queryTruncated = pulseQueries.data?.truncated ?? false
  const runTruncated = (pulseRuns.data?.runs?.length ?? 0) >= PULSE_RUNS_LIMIT
  const queryStart = coveredStart(queryDays, oldestOf((pulseQueries.data?.items ?? []).map((q) => q.createdAt)), queryTruncated)
  const runStart = coveredStart(
    runDays,
    oldestOf((pulseRuns.data?.runs ?? []).map((r) => (r.startedAt === null ? null : Date.parse(r.startedAt)))),
    runTruncated,
  )

  return (
    <div className="space-y-6 max-w-6xl">
      <header className="flex items-end justify-between gap-4 flex-wrap">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Overview</h1>
          <p className="text-text-dim text-sm mt-1">
            Your catalogs, how fresh they are, and the last two weeks of queries and runs.
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
          count={totals?.catalogs}
          hint={totals ? `${formatCount(totals.schemas)} schemas` : metrics.isError ? metricsFailure : 'catalogs you can see'}
          icon="database"
        />
        <Stat
          label="Tables"
          value={totals ? formatCount(totals.tables) : metrics.isError ? '—' : '…'}
          count={totals?.tables}
          hint={totals ? `${formatBytes(totals.sizeBytes)} · ${countRows(totals.records)}` : metrics.isError ? metricsFailure : 'across your catalogs'}
          icon="table"
        />
        {admin && (
          <Stat label="Engine" value={String(engineState)} hint="Trino" icon="spark" tone={engineState === 'ACTIVE' ? 'ok' : undefined} />
        )}
        <Stat label="Activity" value={jobs.data ? String(jobs.data.length) : jobs.isError ? '—' : '…'} count={jobs.data?.length} hint="recent queries, loads and transfers" icon="clock" />
      </section>

      <section className="grid md:grid-cols-2 gap-4">
        <PulseCard
          queries={pulseQueries}
          runs={pulseRuns}
          queryCounts={queryCounts.slice(queryStart)}
          queryDays={queryDays.slice(queryStart)}
          runBuckets={runBuckets.slice(runStart)}
          runDays={runDays.slice(runStart)}
        />
      </section>

      <section>
        <FreshnessCard />
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

      <section ref={timeline.ref} className="grid lg:grid-cols-3 gap-4">
        <div className={`card p-5 ${admin ? 'lg:col-span-2' : 'lg:col-span-3'}`}>
          <div className="flex items-center justify-between gap-3 mb-3 flex-wrap">
            <h2 className="text-sm font-semibold">Recent activity</h2>
            <div className="flex items-center gap-2">
              <div role="group" aria-label="Whose activity" className="flex rounded-[10px] border border-border overflow-hidden text-xs">
                {(['me', 'groups'] as const).map(s => (
                  <button
                    key={s}
                    type="button"
                    aria-pressed={scope === s}
                    onClick={() => setScope(s)}
                    className={`px-2.5 py-1 ${scope === s ? 'bg-accent-solid text-white' : 'bg-bg-elev/60 text-text-dim hover:text-text'}`}
                  >
                    {s === 'me' ? 'Mine' : 'My groups'}
                  </button>
                ))}
              </div>
              <Link to="/runs" className="text-xs text-text-dim hover:text-text">All runs →</Link>
            </div>
          </div>
          {allLoading && (
            <div className="space-y-2">
              {[0, 1, 2].map(i => <Skeleton key={i} className="h-8" />)}
            </div>
          )}
          {allFailed && <ErrorBox error={runs.error} onRetry={() => { void runs.refetch(); void queries.refetch(); void jobs.refetch() }} />}
          {!allLoading && !allFailed && recent.length === 0 && (
            <EmptyState icon="pulse" title={scope === 'me' ? 'Nothing of yours has happened yet' : 'Nothing has happened yet'}>
              {scope === 'me' ? 'Run a query or load a file. Pipeline runs belong to a group — switch to My groups.' : 'Run a query, start a pipeline, or load a file.'}
            </EmptyState>
          )}
          <div className="space-y-1">
            {recent.map(row => (
              <Link
                key={row.id}
                to={row.href}
                data-at={row.at}
                aria-label={row.detail ? `${row.title} — ${row.detail}` : row.title}
                className="flex items-center gap-3 py-2 border-b border-border/60 last:border-0 hover:bg-bg-soft/40 rounded px-2 -mx-2"
              >
                <span className={kindBadge(row.kind)}>{row.kind}</span>
                <span className="text-sm truncate flex-1">{row.title}</span>
                {row.detail && <span className="text-xs text-text-mute">{row.detail}</span>}
                <time
                  dateTime={new Date(row.at).toISOString()}
                  className="text-xs text-text-mute w-36 text-right"
                >
                  {relativeTime(row.at)}
                </time>
              </Link>
            ))}
          </div>
          <p className="text-xs text-text-dim mt-3">
            Loads are always yours. Pipeline runs and their scheduled queries belong to their group.
          </p>
          {failed.map(source => (
            <p key={source.key} className="text-xs text-text-dim mt-2 flex items-center gap-2">
              <span>Could not read {source.label}.</span>
              <button type="button" className="btn-ghost px-2 py-0.5 text-xs" onClick={() => void source.query.refetch()}>
                Retry
              </button>
            </p>
          ))}
        </div>

        {admin && (
        <div className="card p-5">
          <div className="flex items-center justify-between mb-3">
            <h2 className="text-sm font-semibold">Platform apps</h2>
            <Link to="/platform" className="text-xs text-text-dim hover:text-text">All apps →</Link>
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
          <Action to="/platform" icon="grid" title="Platform" body="Every service, live status and deep links." />
        )}
      </section>
    </div>
  )
}

/**
 * "The last two weeks" (#767): how many queries the person ran per day, and how their runs
 * ended. The query log is read page by page over the window, up to 4,000 rows (nova#883); the
 * runs read is one page of 200. A shortened answer says so instead of pretending the window is
 * complete. A failing source hides only its own chart, and a source still loading reserves its
 * height instead of flashing a spinner.
 */
function PulseCard({ queries, runs, queryCounts, queryDays, runBuckets, runDays }: {
  queries: UseQueryResult<{ items: QueryLogItem[]; truncated: boolean }>
  runs: UseQueryResult<RunsListResponse>
  queryCounts: number[]
  queryDays: number[]
  runBuckets: RunBucket[]
  runDays: number[]
}) {
  const runItems = runs.data?.runs ?? []
  const queryTotal = queryCounts.reduce((sum, c) => sum + c, 0)
  const runTotal = runBuckets.reduce((sum, b) => sum + b.ok + b.failed + b.running + b.other, 0)
  const bothFailed = queries.isError && runs.isError
  // "No rows in the window" is a claim about both answers: nothing while one is still out.
  const empty = queries.data !== undefined && runs.data !== undefined && queryTotal === 0 && runTotal === 0
  // 326 px = the empty state at 1280×720 (#860, measured 2026-11-12); the loading state
  // (two 2-row skeletons) is 171 px and must not be shorter, or the cards below move. The
  // chart's axis and legend (#881) leave the drawn card under that, so the minimum stays.
  return (
    <div className="card p-5 md:col-span-2 min-w-0 min-h-[326px]">
      <h2 className="text-sm font-semibold">The last two weeks</h2>
      {bothFailed && (
        <div className="mt-3">
          <ErrorBox error={queries.error} onRetry={() => { void queries.refetch(); void runs.refetch() }} />
        </div>
      )}
      {!bothFailed && empty && (
        <EmptyState icon="pulse" title="No queries or runs yet">Run a query or start a pipeline and the shape of your week shows up here.</EmptyState>
      )}
      {!bothFailed && !empty && (
        <div className="grid md:grid-cols-2 gap-6 mt-3">
          <div className="min-w-0">
            <div className="flex items-baseline justify-between gap-2">
              <span className="label">Queries per day · just you</span>
              <span className="text-xs text-text-mute">14 days · {formatCount(queryTotal)} queries</span>
            </div>
            {queries.isError ? (
              <p className="text-xs text-text-dim mt-2 flex items-center gap-2">
                <span>Could not read queries.</span>
                <button type="button" className="btn-ghost px-2 py-0.5 text-xs" onClick={() => void queries.refetch()}>
                  Retry
                </button>
              </p>
            ) : queries.data === undefined ? (
              <div className="mt-2"><SkeletonTable rows={2} /></div>
            ) : (
              <QuerySparkline counts={queryCounts} days={queryDays} scope="last 14 days" />
            )}
            {queries.data?.truncated && (
              <p className="text-xs text-text-mute mt-1">Showing the newest 4,000 queries.</p>
            )}
          </div>
          <div className="min-w-0">
            <div className="flex items-baseline justify-between gap-2">
              <span className="label">Runs per day · your groups</span>
              <span className="text-xs text-text-mute">7 days · {formatCount(runTotal)} runs</span>
            </div>
            {runs.isError ? (
              <p className="text-xs text-text-dim mt-2 flex items-center gap-2">
                <span>Could not read runs.</span>
                <button type="button" className="btn-ghost px-2 py-0.5 text-xs" onClick={() => void runs.refetch()}>
                  Retry
                </button>
              </p>
            ) : runs.data === undefined ? (
              <div className="mt-2"><SkeletonTable rows={2} /></div>
            ) : (
              <RunBars buckets={runBuckets} days={runDays} scope="last 7 days" />
            )}
            {runItems.length === 200 && (
              <p className="text-xs text-text-mute mt-1">Showing the latest 200 runs.</p>
            )}
          </div>
        </div>
      )}
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
      {!metrics && !error && (
        <>
          {/* The table's header line: the loading branch reserves the header too (#860).
              A plain block, not a `Skeleton`, so the row count stays the eight rows below. */}
          <div aria-hidden="true" className="mt-1 mb-2 h-3 w-24 rounded-lg bg-bg-soft" />
          {/* 29 px = the measured `tbody tr` (37 px) minus the 8 px `space-y-2` gap. */}
          <SkeletonTable rows={8} rowHeight={29} />
        </>
      )}
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
    'pip install "stardelt @ git+https://github.com/stardelt/stardelt-python@v0.2.0"',
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

/** The number a count tile shows: 0 → target over 400 ms, once per mount (#512 item 4).
 *  A later answer (the 10 s refresh) is shown at once; reduced motion is shown at once. */
function useCountUp(target: number | undefined): number | undefined {
  const [shown, setShown] = useState<number | undefined>(undefined)
  const started = useRef(false)
  useEffect(() => {
    if (target === undefined) {
      setShown(undefined)
      return
    }
    if (started.current || window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
      started.current = true
      setShown(target)
      return
    }
    started.current = true
    const began = performance.now()
    let frame = 0
    const tick = (now: number) => {
      const done = Math.min(1, (now - began) / 400)
      setShown(Math.round(target * done))
      if (done < 1) frame = requestAnimationFrame(tick)
    }
    frame = requestAnimationFrame(tick)
    return () => cancelAnimationFrame(frame)
  }, [target])
  return shown
}

function Stat({ label, value, count, hint, icon, tone }: {
  label: string
  value: string
  /** When given, the tile counts 0 → this number once (#512 item 4). */
  count?: number
  hint: string
  icon: 'table' | 'spark' | 'clock' | 'database'
  tone?: 'ok'
}) {
  const shown = useCountUp(count)
  const text = count === undefined ? value : formatCount(shown ?? 0)
  return (
    <div className="card p-4">
      <div className="flex items-center justify-between">
        <span className="label">{label}</span>
        <Icon name={icon} className="w-4 h-4 text-text-mute" />
      </div>
      <div className={`text-2xl font-semibold mt-2 flex items-center gap-2 lowercase first-letter:uppercase ${tone === 'ok' ? 'text-text' : ''}`}>
        {tone === 'ok' && <span className="w-2 h-2 rounded-full bg-success shrink-0" aria-hidden="true" />}
        {text}
      </div>
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

function kindBadge(kind: ActivityRow['kind']) {
  const map: Record<ActivityRow['kind'], string> = {
    pipeline: 'badge border-violet-400/40 bg-violet-400/10 text-xfer',
    notebook: 'badge border-amber-400/40 bg-amber-400/10 text-warn',
    query: 'badge border-sky-400/40 bg-sky-400/10 text-info',
    load: 'badge border-emerald-400/40 bg-emerald-400/10 text-ok',
  }
  return map[kind] ?? 'badge border-border bg-bg-soft text-text-dim'
}
