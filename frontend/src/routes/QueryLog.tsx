import { useEffect, useMemo, useRef, useState } from 'react'
import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api, errorMessage, type QueryLogItem } from '../api'
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

  const [scope, setScope] = useState<'mine' | 'all'>('mine')
  const [state, setState] = useState<StateFilter>('')
  const [rangeIdx, setRangeIdx] = useState(1)
  const [search, setSearch] = useState('')
  const q = useDebounced(search.trim(), 300)
  const [columns, setColumns] = useState<ColumnId[]>(loadColumns)
  const [open, setOpen] = useState<string | null>(null)
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

  // A person who is not an administrator never has the all scope, whatever the state says.
  const effectiveScope = admin ? scope : 'mine'
  const range = RANGES[rangeIdx]

  const log = useInfiniteQuery({
    queryKey: ['query-log', effectiveScope, state, rangeIdx, q],
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
    if (effectiveScope === 'all') extra.add('user')
    return extra
  }, [columns, effectiveScope])
  // The expander, the six default columns and whatever the person added.
  const colCount = 7 + shown.size

  function toggleColumn(id: ColumnId) {
    const next = columns.includes(id) ? columns.filter(c => c !== id) : [...columns, id]
    setColumns(next)
    saveColumns(next)
  }

  return (
    <div className="max-w-6xl space-y-5">
      <header className="flex items-end justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Query log</h1>
          <p className="text-text-dim text-sm mt-1">
            {effectiveScope === 'all' ? 'Queries of everyone, newest first.' : 'The queries you ran, newest first.'}
          </p>
        </div>
        <div className="text-xs text-text-mute flex items-center gap-2" aria-live="polite">
          <span className="w-1.5 h-1.5 rounded-full bg-success animate-pulse" />
          {log.isFetching && !log.isLoading ? 'updating…' : 'live'}
        </div>
      </header>

      <div className="flex flex-wrap items-center gap-3">
        {admin && (
          <div role="group" aria-label="Whose queries" className="flex rounded-[10px] border border-border overflow-hidden text-sm">
            {(['mine', 'all'] as const).map(s => (
              <button
                key={s}
                type="button"
                aria-pressed={scope === s}
                onClick={() => setScope(s)}
                className={`px-3 py-1.5 ${scope === s ? 'bg-accent-solid text-white' : 'bg-bg-elev/60 text-text-dim hover:text-text'}`}
              >
                {s === 'mine' ? 'Mine' : 'Everyone'}
              </button>
            ))}
          </div>
        )}
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
              <th className="th px-2 py-2.5 w-8"><span className="sr-only">Details</span></th>
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
            {log.isError && !log.data && (
              <tr>
                <td className="px-4 py-5 text-bad" colSpan={colCount}>
                  {errorMessage(log.error)}{' '}
                  <button type="button" className="underline" onClick={() => log.refetch()}>Retry</button>
                </td>
              </tr>
            )}
            {log.data && items.length === 0 && (
              <tr>
                <td className="px-4 py-6 text-text-mute" colSpan={colCount}>
                  No queries match. Run one in the <Link className="underline" to="/query">SQL editor</Link>, or widen the time range.
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
                onToggle={() => setOpen(open === item.id ? null : item.id)}
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

