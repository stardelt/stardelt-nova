import { useEffect, useRef, useState, type KeyboardEvent } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link, useSearchParams } from 'react-router-dom'
import { api, allTables, downloadCsv, errorMessage, formatCell, type QueryResult, type TableRef } from '../api'
import CatalogSelect, { useCatalogs } from '../components/CatalogSelect'
import { Icon } from '../components/Icon'
import ScheduleDialog from '../components/ScheduleDialog'
import { formatCount, sqlTableRef } from '../format'

const HISTORY_KEY = 'stardelt-nova:history'
const HISTORY_LIMIT = 50

/**
 * A sample that exists: the NYC demo table if present, else the first table, else a
 * constant query. With a catalog, the statement names that catalog, so the person's
 * chosen catalog is the one the editor opens in (stardelt-nova#234).
 */
function pickSample(tables: TableRef[] | undefined, catalog?: string): string {
  if (!tables || tables.length === 0) return 'SELECT 1'
  const t = tables.find(x => x.ns === 'nyc_taxi' && x.name === 'yellow_trips') ?? tables[0]
  return `SELECT * FROM ${sqlTableRef(t.ns, t.name, catalog)} LIMIT 100`
}

function loadHistory(): string[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(HISTORY_KEY) ?? '[]')
    // Stored data is untrusted: anything but an array of strings would crash `.filter` / `.slice` below.
    return Array.isArray(parsed) ? parsed.filter((h): h is string => typeof h === 'string') : []
  } catch {
    return []
  }
}
function saveHistory(h: string[]) {
  // Persisting history is a convenience; blocked or full storage must not fail the query.
  try {
    localStorage.setItem(HISTORY_KEY, JSON.stringify(h.slice(0, HISTORY_LIMIT)))
  } catch {
    /* ignore */
  }
}

export default function Query() {
  const [params] = useSearchParams()
  // The catalog the person works in. It names the catalogs the sample is generated from,
  // and the Catalog page's "Query table" link carries it along (stardelt-nova#234).
  const catalog = params.get('catalog') ?? undefined
  const catalogs = useCatalogs()
  const tables = useQuery({
    queryKey: ['catalog', 'all', catalog],
    queryFn: () => allTables(catalog),
  })
  const sample = pickSample(tables.data, catalog)
  // `null` = the user has not typed or picked anything: show the sample.
  const [typed, setTyped] = useState<string | null>(params.get('sql'))
  const sql = typed ?? sample
  const [running, setRunning] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [result, setResult] = useState<QueryResult | null>(null)
  const [elapsed, setElapsed] = useState<number | null>(null)
  const [history, setHistory] = useState<string[]>(loadHistory)
  const [scheduling, setScheduling] = useState(false)
  const [scheduled, setScheduled] = useState<{ name: string; namespace: string } | null>(null)
  const taRef = useRef<HTMLTextAreaElement>(null)
  const empty = sql.trim() === ''

  useEffect(() => {
    const fromUrl = params.get('sql')
    if (fromUrl) setTyped(fromUrl)
  }, [params])

  async function run() {
    if (running) return // Ctrl+Enter bypasses the disabled button
    setRunning(true); setError(null); setResult(null); setElapsed(null)
    const t0 = performance.now()
    try {
      const r = await api.query(sql)
      setResult(r)
      setElapsed(performance.now() - t0)
      const next = [sql, ...history.filter(h => h !== sql)].slice(0, HISTORY_LIMIT)
      setHistory(next); saveHistory(next)
    } catch (e: unknown) {
      setError(errorMessage(e))
      setElapsed(performance.now() - t0)
    } finally {
      setRunning(false)
    }
  }

  function onKey(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
      e.preventDefault()
      run()
    }
  }

  return (
    <div className="grid grid-cols-1 lg:grid-cols-[minmax(0,1fr)_280px] gap-5 h-full min-h-0">
      <h1 className="sr-only">SQL editor</h1>
      <div className="flex flex-col gap-4 min-h-0 min-w-0">
        {(catalogs.data?.catalogs?.length ?? 0) > 1 && (
          <div>
            <label className="grid gap-1.5">
              <span className="label">Catalog</span>
              <CatalogSelect />
            </label>
            {/* The choice scopes what Nova generates; it cannot change Trino's session
                default, and a statement that names its catalog works wherever it is pasted. */}
            <p className="text-xs text-text-mute mt-1">Samples and inserted table names use this catalog.</p>
          </div>
        )}
        <div className="card p-3 focus-within:border-accent/60">
          <textarea
            ref={taRef}
            value={sql}
            onChange={e => setTyped(e.target.value)}
            onKeyDown={onKey}
            aria-label="SQL query"
            className="w-full h-36 bg-transparent font-mono text-sm outline-hidden resize-y placeholder:text-text-mute"
            spellCheck={false}
            placeholder="SELECT ..."
          />
          <div className="flex items-center gap-2 mt-2">
            <div className="text-xs text-text-mute">⌘/Ctrl+Enter to run</div>
            <button onClick={() => setTyped(null)} className="btn-ghost ml-auto py-1.5">Sample</button>
            <button
              type="button"
              onClick={() => setScheduling(true)}
              disabled={empty}
              title={empty ? 'Write a statement first' : undefined}
              aria-label="Schedule this statement"
              className="btn-ghost py-1.5"
            >
              <Icon name="clock" className="w-4 h-4" />
              Schedule
            </button>
            <button onClick={run} disabled={running} className="btn-primary py-1.5">
              <Icon name="terminal" className="w-4 h-4" />
              {running ? 'Running…' : 'Run'}
            </button>
          </div>
        </div>

        {scheduled && (
          <p role="status" className="text-sm text-ok break-words">
            Scheduled{' '}
            <Link
              to={`/pipelines/${encodeURIComponent(scheduled.namespace)}/${encodeURIComponent(scheduled.name)}`}
              className="underline"
            >
              {scheduled.name}
            </Link>
            . It appears under Pipelines within a minute.
          </p>
        )}

        {error && (
          <div className="rounded-[14px] border border-red-500/40 bg-red-500/10 p-4 text-sm text-bad font-mono whitespace-pre-wrap break-words">
            {error}
          </div>
        )}

        {result && (
          <div className="card flex-1 min-h-0 overflow-hidden flex flex-col">
            <div className="px-4 py-2 text-xs text-text-mute border-b border-border flex items-center gap-3">
              <span>{result.row_count} row{result.row_count === 1 ? '' : 's'}</span>
              {elapsed != null && <span>· {(elapsed / 1000).toFixed(2)}s</span>}
              <span className="font-mono truncate">{result.query_id}</span>
              <button
                onClick={() => downloadCsv(`nova-query-${result.query_id ?? 'result'}.csv`, result.columns.map(c => c.name), result.rows)}
                className="btn-ghost ml-auto py-1 px-2 text-xs"
                title={result.truncated ? 'This CSV holds only the first rows: add a LIMIT or aggregate to see the rest.' : 'Download these rows as CSV'}
              >
                <Icon name="download" className="w-3.5 h-3.5" />
                CSV
              </button>
            </div>
            {result.truncated && (
              <div className="px-4 py-2 text-xs bg-amber-500/10 border-b border-amber-500/40 text-amber-600 dark:text-amber-400">
                Showing the first {formatCount(result.row_count)} rows. Add a LIMIT or aggregate to see the rest.
              </div>
            )}
            <div className="overflow-auto flex-1">
              <table className="w-full text-sm font-mono">
                <thead className="text-text-mute sticky top-0 bg-bg-elev">
                  <tr>{result.columns.map((c, i) => <th key={i} className="th px-4 py-2">{c.name}</th>)}</tr>
                </thead>
                <tbody>
                  {result.rows.map((row, i) => (
                    <tr key={i} className="border-t border-border/60">
                      {row.map((cell, j) => <td key={j} className="td px-4 text-text-dim">{formatCell(cell)}</td>)}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </div>
        )}
      </div>

      <aside className="card p-4 overflow-auto">
        <div className="flex items-center justify-between mb-3">
          <h3 className="label">History</h3>
          <button
            onClick={() => { setHistory([]); saveHistory([]) }}
            className="text-xs text-text-mute hover:text-text"
          >clear</button>
        </div>
        {history.length === 0 && <div className="text-text-mute text-sm">no queries yet</div>}
        <ul className="space-y-2">
          {history.map((h, i) => (
            <li key={i}>
              <button
                onClick={() => { setTyped(h); taRef.current?.focus() }}
                className="w-full text-left text-xs font-mono text-text-dim hover:text-text break-all"
                title={h}
              >
                {h.length > 60 ? h.slice(0, 60) + '…' : h}
              </button>
            </li>
          ))}
        </ul>
      </aside>

      {scheduling && (
        <ScheduleDialog
          mode="create"
          sql={sql}
          onClose={() => setScheduling(false)}
          onScheduled={(view) => {
            setScheduling(false)
            setScheduled({ name: view.name, namespace: view.namespace })
          }}
        />
      )}
    </div>
  )
}
