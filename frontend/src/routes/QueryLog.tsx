import { useEffect, useMemo, useRef, useState } from 'react'
import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { Link, useSearchParams } from 'react-router-dom'
import { api, errorMessage, type QueryLogItem, type QueryProfileOperator } from '../api'
import { EmptyState } from '../components/EmptyState'
import { formatBytes, formatCount, formatDurationMs, formatTime, relativeTime, rowsLabel, sourceLabel } from '../format'
import { isAdmin } from '../roles'

/** Query log: the queries a person ran, simple by default, every metric one click away. */

type StateFilter = '' | 'QUEUED' | 'FINISHED' | 'FAILED' | 'CANCELED'

const STATE_CHIPS: { value: StateFilter; label: string }[] = [
  { value: '', label: 'All' },
  { value: 'QUEUED', label: 'In progress' },
  { value: 'FINISHED', label: 'Succeeded' },
  { value: 'FAILED', label: 'Failed' },
  { value: 'CANCELED', label: 'Canceled' },
]

const RANGES: { label: string; ms: number | null }[] = [
  { label: 'Last hour', ms: 3_600_000 },
  { label: 'Last 24 hours', ms: 86_400_000 },
  { label: 'Last 7 days', ms: 7 * 86_400_000 },
  { label: 'Last 30 days', ms: 30 * 86_400_000 },
  { label: 'All time', ms: null },
]

type ColumnId = 'user' | 'source' | 'type' | 'id' | 'error'

/** Columns a person can add; the six default ones are always there. */
const OPTIONAL_COLUMNS: { id: ColumnId; label: string }[] = [
  { id: 'user', label: 'User' },
  { id: 'source', label: 'Source' },
  { id: 'type', label: 'Statement type' },
  { id: 'error', label: 'Error' },
  { id: 'id', label: 'Query id' },
]

const COLUMNS_KEY = 'nova.queryLog.columns'

function loadColumns(): ColumnId[] {
  try {
    const raw = localStorage.getItem(COLUMNS_KEY)
    const parsed: unknown = raw ? JSON.parse(raw) : []
    const known = new Set<string>(OPTIONAL_COLUMNS.map(c => c.id))
    return Array.isArray(parsed) ? (parsed.filter(c => typeof c === 'string' && known.has(c)) as ColumnId[]) : []
  } catch {
    return []
  }
}

function saveColumns(columns: ColumnId[]) {
  try {
    localStorage.setItem(COLUMNS_KEY, JSON.stringify(columns))
  } catch {
    // Private windows and blocked storage: the choice just does not persist.
  }
}

function useDebounced<T>(value: T, ms: number): T {
  const [debounced, setDebounced] = useState(value)
  useEffect(() => {
    const t = setTimeout(() => setDebounced(value), ms)
    return () => clearTimeout(t)
  }, [value, ms])
  return debounced
}

export default function QueryLog() {
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const admin = isAdmin(me.data)
  // Demo mode has no state store, so the log route answers 503 (#500). Do not ask.
  const demo = me.data?.demo === true

  const [scope, setScope] = useState<'mine' | 'team' | 'all'>('mine')
  const [state, setState] = useState<StateFilter>('')
  const [rangeIdx, setRangeIdx] = useState(1)
  const [search, setSearch] = useState('')
  const q = useDebounced(search.trim(), 300)
  const [columns, setColumns] = useState<ColumnId[]>(loadColumns)
  const [params, setParams] = useSearchParams()
  const [open, setOpen] = useState<string | null>(() => params.get('id'))
  const columnsMenu = useRef<HTMLDetailsElement>(null)

  // A click elsewhere on the page dismisses the menu, like the header's app switcher.
  useEffect(() => {
    const onMouseDown = (e: MouseEvent) => {
      const menu = columnsMenu.current
      if (menu?.open && !menu.contains(e.target as Node)) menu.open = false
    }
    document.addEventListener('mousedown', onMouseDown)
    return () => document.removeEventListener('mousedown', onMouseDown)
  }, [])

  // The all scope is for administrators; team is for every signed-in person (the API decides
  // which rows their groups own, and answers an empty page when none are).
  const effectiveScope = admin || scope !== 'all' ? scope : 'mine'
  const range = RANGES[rangeIdx]

  const log = useInfiniteQuery({
    queryKey: ['query-log', effectiveScope, state, rangeIdx, q],
    // Wait for /api/me first: on the first render it is still in flight, and `enabled` would
    // otherwise be true in demo mode and ask the 503 route once anyway.
    enabled: me.isSuccess && me.data?.demo !== true,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      api.queryLog({
        scope: effectiveScope,
        state: state || undefined,
        q: q || undefined,
        from: range.ms == null ? undefined : Date.now() - range.ms,
        cursor: pageParam,
      }),
    getNextPageParam: page => page.nextCursor ?? undefined,
    // Quick while something is still running, relaxed otherwise.
    refetchInterval: query =>
      query.state.data?.pages.some(p => p.items.some(i => i.state === 'QUEUED')) ? 5_000 : 30_000,
  })

  const items = useMemo(() => (log.data?.pages ?? []).flatMap(p => p.items), [log.data])
  const shown = useMemo(() => {
    const extra = new Set<ColumnId>(columns)
    if (effectiveScope !== 'mine') extra.add('user')
    return extra
  }, [columns, effectiveScope])
  // The expander, the six default columns and whatever the person added.
  const colCount = 7 + shown.size

  function toggleColumn(id: ColumnId) {
    const next = columns.includes(id) ? columns.filter(c => c !== id) : [...columns, id]
    setColumns(next)
    saveColumns(next)
  }

  /** Open or close one query's details, and put the open one in the address (nova#509). */
  function toggleOpen(id: string) {
    const nextId = open === id ? null : id
    setOpen(nextId)
    const next = new URLSearchParams(params)
    if (nextId) next.set('id', nextId)
    else next.delete('id')
    setParams(next, { replace: true })
  }

  return (
    <div className="max-w-6xl space-y-5">
      <header className="flex items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Query log</h1>
          <p className="text-text-dim text-sm mt-1">
            {effectiveScope === 'all'
              ? 'Queries of everyone, newest first.'
              : effectiveScope === 'team'
                ? 'Queries run by the service principals your groups own, newest first.'
                : 'The queries you ran, newest first.'}
          </p>
        </div>
        <div className="text-xs text-text-mute flex items-center gap-2" aria-live="polite">
          <span className="w-1.5 h-1.5 rounded-full bg-success animate-pulse" />
          {log.isFetching && !log.isLoading ? 'updating…' : 'live'}
        </div>
      </header>

      <div className="flex flex-wrap items-center gap-3">
        <div role="group" aria-label="Whose queries" className="flex rounded-[10px] border border-border overflow-hidden text-sm">
          {(['mine', 'team', 'all'] as const)
            .filter(s => s !== 'all' || admin)
            .map(s => (
              <button
                key={s}
                type="button"
                aria-pressed={scope === s}
                onClick={() => setScope(s)}
                className={`px-3 py-1.5 ${scope === s ? 'bg-accent-solid text-white' : 'bg-bg-elev/60 text-text-dim hover:text-text'}`}
              >
                {s === 'mine' ? 'Mine' : s === 'team' ? 'Team' : 'Everyone'}
              </button>
            ))}
        </div>
        <div role="group" aria-label="Status" className="flex flex-wrap gap-1.5">
          {STATE_CHIPS.map(c => (
            <button
              key={c.value || 'all'}
              type="button"
              aria-pressed={state === c.value}
              onClick={() => setState(c.value)}
              className={`inline-flex items-center rounded-full border px-3 py-1 text-xs font-medium ${state === c.value ? 'border-accent/60 bg-accent/15 text-text' : 'border-border bg-bg-elev/60 text-text-dim hover:text-text'}`}
            >
              {c.label}
            </button>
          ))}
        </div>
        <select
          aria-label="Time range"
          value={rangeIdx}
          onChange={e => setRangeIdx(Number(e.target.value))}
          className="input !w-auto py-1.5"
        >
          {RANGES.map((r, i) => (
            <option key={r.label} value={i}>{r.label}</option>
          ))}
        </select>
        <input
          type="search"
          aria-label="Search statements and query ids"
          placeholder="Search statements or a query id"
          value={search}
          onChange={e => setSearch(e.target.value)}
          className="input !w-64 py-1.5"
        />
        <details
          ref={columnsMenu}
          className="relative ml-auto"
          onKeyDown={e => {
            const menu = columnsMenu.current
            if (e.key === 'Escape' && menu?.open) {
              menu.open = false
              menu.querySelector('summary')?.focus()
            }
          }}
        >
          <summary className="btn-ghost py-1.5 cursor-pointer list-none">Columns</summary>
          <div className="absolute right-0 z-10 mt-2 w-48 card p-2 space-y-1 shadow-lg bg-bg-elev">
            {OPTIONAL_COLUMNS.map(c => {
              const forced = c.id === 'user' && effectiveScope === 'all'
              return (
                <label key={c.id} className="flex items-center gap-2 px-2 py-1 text-sm rounded hover:bg-bg-soft/60">
                  <input
                    type="checkbox"
                    checked={shown.has(c.id)}
                    disabled={forced}
                    onChange={() => toggleColumn(c.id)}
                  />
                  {c.label}
                </label>
              )
            })}
          </div>
        </details>
      </div>

      {/* The container makes `cqw` below the visible width of the card, however wide the table is. */}
      <div className="[container-type:inline-size]">
      <div className="card overflow-x-auto">
        <table className="w-full text-sm">
          <caption className="sr-only">Queries, newest first. Expand a row for its details.</caption>
          <thead className="bg-bg-soft/50">
            <tr>
              <th className="th px-2 py-2.5 w-8"><span className="sr-only">Show query details</span></th>
              <th className="th px-3 py-2.5">Status</th>
              <th className="th px-3 py-2.5">Query</th>
              <th className="th px-3 py-2.5">Warehouse</th>
              <th className="th px-3 py-2.5 text-right">Started</th>
              <th className="th px-3 py-2.5 text-right">Duration</th>
              <th className="th px-3 py-2.5 text-right">Rows</th>
              {shown.has('user') && <th className="th px-3 py-2.5">User</th>}
              {shown.has('source') && <th className="th px-3 py-2.5">Source</th>}
              {shown.has('type') && <th className="th px-3 py-2.5">Type</th>}
              {shown.has('error') && <th className="th px-3 py-2.5">Error</th>}
              {shown.has('id') && <th className="th px-3 py-2.5">Query id</th>}
            </tr>
          </thead>
          <tbody>
            {log.isLoading && (
              <tr><td className="px-4 py-5 text-text-mute" colSpan={colCount}>loading…</td></tr>
            )}
            {demo && (
              <tr>
                <td className="px-4 py-6 text-text-mute" colSpan={colCount}>
                  Demo mode keeps no query history. Run a query in the <Link className="underline" to="/query">SQL editor</Link> to see one in a real install.
                </td>
              </tr>
            )}
            {!demo && log.isError && !log.data && (
              <tr>
                <td className="px-4 py-5 text-bad" colSpan={colCount}>
                  {errorMessage(log.error)}{' '}
                  <button type="button" className="underline" onClick={() => log.refetch()}>Retry</button>
                </td>
              </tr>
            )}
            {log.data && items.length === 0 && (
              <tr>
                <td colSpan={colCount}>
                  <EmptyState icon="search" title="No queries match">
                    Run one in the <Link className="underline" to="/query">SQL editor</Link>, or widen the time range.
                  </EmptyState>
                </td>
              </tr>
            )}
            {items.map(item => (
              <Row
                key={item.id}
                item={item}
                shown={shown}
                colCount={colCount}
                expanded={open === item.id}
                onToggle={() => toggleOpen(item.id)}
              />
            ))}
          </tbody>
        </table>
      </div>
      </div>

      {log.hasNextPage && (
        <div className="flex justify-center">
          <button type="button" className="btn-ghost" disabled={log.isFetchingNextPage} onClick={() => log.fetchNextPage()}>
            {log.isFetchingNextPage ? 'Loading…' : 'Load more'}
          </button>
        </div>
      )}
    </div>
  )
}

function Row({
  item, shown, colCount, expanded, onToggle,
}: {
  item: QueryLogItem
  shown: Set<ColumnId>
  colCount: number
  expanded: boolean
  onToggle: () => void
}) {
  const panelId = `ql-${item.id}`
  const firstLine = item.sql.split('\n')[0]
  return (
    <>
      <tr className="border-t border-border/70 hover:bg-bg-soft/30">
        <td className="td px-2">
          <button
            type="button"
            aria-expanded={expanded}
            aria-controls={panelId}
            aria-label={expanded ? 'Hide details' : 'Show details'}
            onClick={onToggle}
            className="w-6 h-6 rounded text-text-dim hover:text-text hover:bg-bg-soft/60"
          >
            <span aria-hidden className={`inline-block transition-transform ${expanded ? 'rotate-90' : ''}`}>›</span>
          </button>
        </td>
        <td className="td px-3 whitespace-nowrap"><Status state={item.state} /></td>
        <td className="td px-3">
          <div className="font-mono text-xs truncate max-w-[480px]" title={item.sql}>{firstLine}</div>
        </td>
        <td className="td px-3 text-text-dim whitespace-nowrap">{item.warehouse}</td>
        <td className="td px-3 text-right text-text-dim text-xs whitespace-nowrap" title={formatTime(item.createdAt)}>
          {relativeTime(item.createdAt)}
        </td>
        <td className="td px-3 text-right text-text-dim whitespace-nowrap">{formatDurationMs(item.wallMs)}</td>
        <td className="td px-3 text-right text-text-dim whitespace-nowrap">
          {item.outputRows != null ? rowsLabel(item.outputRows) : '—'}
        </td>
        {shown.has('user') && (
          <td className="td px-3 text-text-dim whitespace-nowrap" title={item.user}>
            {item.userName ?? item.user}
          </td>
        )}
        {shown.has('source') && <td className="td px-3 text-text-dim whitespace-nowrap">{sourceLabel(item.source)}</td>}
        {shown.has('type') && <td className="td px-3 text-text-dim whitespace-nowrap">{item.queryType ?? '—'}</td>}
        {shown.has('error') && <td className="td px-3 text-bad whitespace-nowrap">{item.errorCode ?? ''}</td>}
        {shown.has('id') && <td className="td px-3 font-mono text-[11px] text-text-mute whitespace-nowrap">{item.id}</td>}
      </tr>
      {expanded && (
        <tr id={panelId} className="bg-bg-soft/20">
          <td colSpan={colCount} className="px-4 py-4">
            {/* Sticky and no wider than the card, so the panel stays in view when the table scrolls sideways. */}
            <div className="sticky left-0 max-w-[calc(100cqw-2.5rem)]">
              <Details id={item.id} />
            </div>
          </td>
        </tr>
      )}
    </>
  )
}

function Status({ state }: { state: string }) {
  const map: Record<string, { label: string; icon: string; cls: string }> = {
    FINISHED: { label: 'Succeeded', icon: '✓', cls: 'border-emerald-400/40 bg-emerald-400/10 text-ok' },
    FAILED: { label: 'Failed', icon: '✕', cls: 'border-red-400/40 bg-red-400/10 text-bad' },
    CANCELED: { label: 'Canceled', icon: '⊘', cls: 'border-amber-400/40 bg-amber-400/10 text-warn' },
    QUEUED: { label: 'In progress', icon: '◔', cls: 'border-sky-400/40 bg-sky-400/10 text-info' },
  }
  const s = map[state] ?? { label: state, icon: '•', cls: 'border-border bg-bg-soft text-text-dim' }
  return (
    <span className={`badge ${s.cls}`}>
      <span aria-hidden>{s.icon}</span>
      {s.label}
    </span>
  )
}

function Details({ id }: { id: string }) {
  const d = useQuery({ queryKey: ['query-log', 'detail', id], queryFn: () => api.queryLogDetail(id) })
  const [copied, setCopied] = useState<string | null>(null)

  async function copy(what: string, text: string) {
    try {
      await navigator.clipboard.writeText(text)
      setCopied(what)
      setTimeout(() => setCopied(null), 1500)
    } catch {
      // Clipboard access can be refused (insecure origin, permissions); nothing to report.
    }
  }

  if (d.isLoading) return <div className="text-text-mute text-sm">loading…</div>
  if (d.isError || !d.data) return <div className="text-bad text-sm">{String(d.error ?? 'not found')}</div>
  const q = d.data
  return (
    <div className="space-y-3">
      <div>
        <div className="flex items-center gap-2 mb-1.5">
          <span className="label">Statement</span>
          <button type="button" className="btn-ghost py-0.5 px-2 text-xs" onClick={() => copy('sql', q.sql)}>
            {copied === 'sql' ? 'Copied' : 'Copy'}
          </button>
          <Link className="btn-ghost py-0.5 px-2 text-xs" to={`/query?sql=${encodeURIComponent(q.sql)}`}>
            Open in SQL editor
          </Link>
        </div>
        <pre className="font-mono text-xs whitespace-pre-wrap break-words max-h-64 overflow-auto rounded-[10px] border border-border bg-bg-soft/60 p-3">{q.sql}</pre>
      </div>

      <dl className="grid grid-cols-2 md:grid-cols-4 gap-x-6 gap-y-2 text-sm">
        <Fact label="Query id">
          <span className="font-mono text-[11px]">{q.id}</span>{' '}
          <button type="button" className="underline text-xs text-text-dim" onClick={() => copy('id', q.id)}>
            {copied === 'id' ? 'copied' : 'copy'}
          </button>
        </Fact>
        <Fact label="Started">{formatTime(q.startedAt ?? q.createdAt)}</Fact>
        <Fact label="User">
          <span title={q.user}>{q.userName ?? q.user}</span>
        </Fact>
        <Fact label="Source">{sourceLabel(q.source)}{q.clientTags.length > 0 && <span className="text-text-mute"> · {q.clientTags.join(', ')}</span>}</Fact>
        <Fact label="Catalog and schema">{[q.catalog, q.schema].filter(Boolean).join('.') || '—'}</Fact>
      </dl>

      {q.error && (
        <details open className="rounded-[10px] border border-red-400/30 bg-red-400/5 px-3 py-2">
          <summary className="cursor-pointer text-sm font-medium text-bad">Error · {q.error.code}</summary>
          {q.error.message && <p className="mt-1 text-sm break-words">{q.error.message}</p>}
        </details>
      )}

      <Group title="Timing" summary={formatDurationMs(q.timing.wallMs)}>
        <Metric label="Waiting in the queue" value={formatDurationMs(q.timing.queuedMs)} />
        <Metric label="Planning" value={formatDurationMs(q.timing.planningMs)} />
        <Metric label="Execution" value={formatDurationMs(q.timing.executionMs)} />
        <Metric label="Total (wall clock)" value={formatDurationMs(q.timing.wallMs)} />
        <Metric label="CPU time" value={formatDurationMs(q.timing.cpuMs)} />
      </Group>
      <Profile id={q.id} />
      <Group title="Data" summary={q.data.outputRows != null ? rowsLabel(q.data.outputRows) : undefined}>
        <Metric label="Rows read" value={num(q.data.inputRows)} />
        <Metric label="Bytes read" value={formatBytes(q.data.inputBytes)} />
        <Metric label="Rows returned" value={num(q.data.outputRows)} />
        <Metric label="Bytes returned" value={formatBytes(q.data.outputBytes)} />
        <Metric label="Rows written" value={num(q.data.writtenRows)} />
        <Metric label="Bytes written" value={formatBytes(q.data.writtenBytes)} />
      </Group>
      <Group title="Memory" summary={formatBytes(q.data.peakMemoryBytes)}>
        <Metric label="Peak memory" value={formatBytes(q.data.peakMemoryBytes)} />
      </Group>
      <Group title="Tables" summary={`${q.tablesRead.length} read · ${q.tablesWritten.length} written`}>
        <TableList label="Read" tables={q.tablesRead} />
        <TableList label="Written" tables={q.tablesWritten} />
      </Group>
    </div>
  )
}

/** "3.74s" -> 3740; "12.59ms" -> 12.59; "0.00ns" -> 0. Unit set the engine prints: ns/µs/us/ms/s/m/h. */
function parseEngineDuration(text: string | undefined | null): number {
  if (!text) return 0
  const m = /^([0-9.]+)\s*(ns|µs|us|ms|s|m|h)$/.exec(text.trim())
  if (!m) return 0
  const v = Number(m[1])
  if (!Number.isFinite(v)) return 0
  switch (m[2]) {
    case 'ns': return v / 1e6
    case 'µs':
    case 'us': return v / 1e3
    case 'ms': return v
    case 's': return v * 1e3
    case 'm': return v * 6e4
    default: return v * 36e5
  }
}

/** The CPU one operator spent: input + finish + output, in milliseconds. */
function operatorCpuMs(op: QueryProfileOperator): number {
  return parseEngineDuration(op.addInputCpu) + parseEngineDuration(op.finishCpu) + parseEngineDuration(op.getOutputCpu)
}

/** Plain words for the engine's operator types (the raw type stays in the title). */
const OPERATOR_WORDS: Record<string, string> = {
  TableScanOperator: 'Read', ScanFilterAndProjectOperator: 'Read and filter',
  FilterAndProjectOperator: 'Filter and compute columns',
  LookupJoinOperator: 'Join', HashBuilderOperator: 'Join: build the lookup side',
  HashAggregationOperator: 'Aggregate', AggregationOperator: 'Aggregate',
  ExchangeOperator: 'Move data between workers', LocalExchangeSinkOperator: 'Move data between workers',
  LocalExchangeSourceOperator: 'Move data between workers', PartitionedOutputOperator: 'Move data between workers',
  TaskOutputOperator: 'Send results', DynamicFilterSourceOperator: 'Dynamic filter',
  OrderByOperator: 'Sort', TopNOperator: 'Top N', LimitOperator: 'Limit',
  TableWriterOperator: 'Write', WindowOperator: 'Window function',
}
function operatorWords(op: QueryProfileOperator, inputs: { catalogName?: string; schema?: string; table?: string }[]): string {
  const words = OPERATOR_WORDS[op.operatorType] ?? op.operatorType.replace(/Operator$/, '')
  const named = inputs.filter(i => i.table)
  if ((op.operatorType === 'TableScanOperator' || op.operatorType === 'ScanFilterAndProjectOperator') && named.length === 1) {
    const t = named[0]
    return `${words} ${[t.catalogName, t.schema, t.table].filter(Boolean).join('.')}`
  }
  return words
}

/** Where a finished query spent its time: the top operators by CPU and the engine's plan. */
function Profile({ id }: { id: string }) {
  const p = useQuery({ queryKey: ['query-log', 'profile', id], queryFn: () => api.queryLogProfile(id) })
  // The same statement's recent runs. A 404 (an old query, someone else's) just hides the block.
  const h = useQuery({ queryKey: ['query-log', 'history', id], queryFn: () => api.queryLogHistory(id), retry: false })
  const rows = useMemo(() => {
    const ops = p.data?.operators ?? []
    const total = ops.reduce((sum, op) => sum + operatorCpuMs(op), 0)
    return [...ops]
      .sort((a, b) => operatorCpuMs(b) - operatorCpuMs(a))
      .slice(0, 5)
      .map((op) => ({ op, cpu: operatorCpuMs(op), share: total > 0 ? operatorCpuMs(op) / total : 0 }))
  }, [p.data])

  if (p.isLoading) {
    return (
      <Group title="Profile" summary="where the time went">
        <div className="col-span-2 md:col-span-3 text-text-mute">loading the profile…</div>
      </Group>
    )
  }
  // A missing profile is normal (a running query, an old one, an engine that sent none).
  if (p.isError || !p.data) {
    return (
      <Group title="Profile" summary="where the time went">
        <div className="col-span-2 md:col-span-3 text-text-mute">{errorMessage(p.error)}</div>
      </Group>
    )
  }

  const plan = p.data.plan ?? ''
  // A count answered from Iceberg metadata reads 0 bytes with a non-zero row count. The
  // list is optional in the answer (a stub, or an older backend), so default it rather
  // than crash the page on `.length`.
  const inputs = p.data.inputs ?? []
  const metadataOnly =
    inputs.length > 0 &&
    inputs.every(i => (i.physicalInputBytes ?? 0) === 0) &&
    inputs.some(i => (i.physicalInputRows ?? 0) > 0)
  return (
    <Group title="Profile" summary="where the time went">
      <div className="col-span-2 md:col-span-3">
        {(p.data.hints ?? []).length > 0 && (
          <ul className="mb-2 space-y-1">
            {(p.data.hints ?? []).map((hint) => (
              <li key={hint.code} className="text-sm">
                <span className="font-medium">{hint.title}</span>{' '}
                <span className="text-text-dim">({hint.evidence})</span>{' '}
                <a href={hint.docsUrl} target="_blank" rel="noopener noreferrer" className="text-accent hover:underline">
                  What to change
                </a>
              </li>
            ))}
          </ul>
        )}
        {metadataOnly && (
          <p className="text-sm text-text-dim mb-2">Answered from table metadata: no data files were read.</p>
        )}
        {rows.length === 0 ? (
          <div className="text-text-mute">The engine sent no operator statistics for this query.</div>
        ) : (
          <ol className="space-y-1">
            {rows.map(({ op, cpu, share }) => (
              <li key={`${op.stageId}-${op.pipelineId}-${op.planNodeId}`} className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5">
                <span className="font-medium" title={op.operatorType}>{operatorWords(op, inputs)}</span>
                <span>{formatDurationMs(cpu)} ({Math.round(share * 100)} %)</span>
                <span className="text-text-dim">
                  {formatCount(op.inputPositions)} → {formatCount(op.outputPositions)} rows
                </span>
                {op.peakTotalMemoryReservation && (
                  <span className="text-text-mute">peak {op.peakTotalMemoryReservation}</span>
                )}
                {op.spilledDataSize && op.spilledDataSize !== '0B' && (
                  <span className="text-text-mute">spilled · {op.spilledDataSize}</span>
                )}
                <div className="basis-full h-1.5 rounded-full bg-bg-soft max-w-md">
                  <div className="h-1.5 rounded-full bg-accent/70" style={{ width: `${Math.max(2, Math.round(share * 100))}%` }} />
                </div>
              </li>
            ))}
          </ol>
        )}
        {p.data.truncated && (
          <p className="mt-2 text-text-mute text-xs">
            The engine sent more operators than fit; the list above is from the part that was kept.
          </p>
        )}
        {(() => {
          const history = h.data
          // A stub or a backend that answers `{}` has no runs; that is "nothing to show", not an error.
          if (!history || !history.runs || history.runs.length < 2) return null
          const mine = history.runs.find((run) => run.id === id)?.wallMs ?? null
          const median = history.medianWallMs
          const slower = history.slower && median && mine ? ` · this run took ${(mine / median).toFixed(1)}× the median` : ''
          return (
            <div className="mt-2">
              <p className="text-sm">
                Same statement, the last {history.runs.length} runs
                {median != null && <> · median {formatDurationMs(median)}</>}
                {slower}
              </p>
              <ul className="text-text-dim text-xs">
                {history.runs.map((run) => (
                  <li key={run.id}>
                    {run.endedAt != null ? relativeTime(run.endedAt) : 'no end time'} · {run.state.toLowerCase()} ·{' '}
                    {formatDurationMs(run.wallMs)} · {formatBytes(run.inputBytes)} read
                  </li>
                ))}
              </ul>
            </div>
          )
        })()}
        {plan.trim() !== '' && (
          <details className="mt-2">
            <summary className="cursor-pointer text-sm">Plan</summary>
            <pre className="font-mono text-[11px] whitespace-pre overflow-auto max-h-96 rounded-[10px] border border-border bg-bg-soft/60 p-3">{plan}</pre>
          </details>
        )}
      </div>
    </Group>
  )
}

function num(n: number | null): string {
  return n == null ? '—' : formatCount(n)
}

function Fact({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <dt className="label">{label}</dt>
      <dd className="mt-0.5">{children}</dd>
    </div>
  )
}

function Group({ title, summary, children }: { title: string; summary?: string; children: React.ReactNode }) {
  return (
    <details className="rounded-[10px] border border-border bg-bg-elev/40 px-3 py-2">
      <summary className="cursor-pointer text-sm font-medium flex items-center justify-between gap-3">
        <span>{title}</span>
        {summary && <span className="text-text-mute font-normal">{summary}</span>}
      </summary>
      <div className="mt-2 grid grid-cols-2 md:grid-cols-3 gap-x-6 gap-y-2 text-sm">{children}</div>
    </details>
  )
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="label">{label}</div>
      <div className="mt-0.5">{value}</div>
    </div>
  )
}

function TableList({ label, tables }: { label: string; tables: string[] }) {
  return (
    <div className="col-span-2 md:col-span-3">
      <div className="label">{label}</div>
      <div className="mt-0.5 font-mono text-xs">{tables.length ? tables.join(', ') : '—'}</div>
    </div>
  )
}

