import { useEffect, useRef, useState, type KeyboardEvent } from 'react'
import { useSearchParams } from 'react-router-dom'
import { api, downloadCsv, formatCell, type QueryResult } from '../api'
import { Icon } from '../components/Icon'

const HISTORY_KEY = 'stardelt-nova:history'
const HISTORY_LIMIT = 50
const SAMPLE = 'SELECT * FROM warehouse.nyc_taxi.yellow_trips LIMIT 100'

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
  const [sql, setSql] = useState<string>(params.get('sql') ?? SAMPLE)
  const [running, setRunning] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [result, setResult] = useState<QueryResult | null>(null)
  const [elapsed, setElapsed] = useState<number | null>(null)
  const [history, setHistory] = useState<string[]>(loadHistory)
  const taRef = useRef<HTMLTextAreaElement>(null)

  useEffect(() => {
    const fromUrl = params.get('sql')
    if (fromUrl) setSql(fromUrl)
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
      setError(e instanceof Error ? e.message : String(e))
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
    <div className="grid grid-cols-[1fr_280px] gap-5 h-full min-h-0">
      <div className="flex flex-col gap-4 min-h-0">
        <div className="card p-3">
          <textarea
            ref={taRef}
            value={sql}
            onChange={e => setSql(e.target.value)}
            onKeyDown={onKey}
            className="w-full h-36 bg-transparent font-mono text-sm outline-hidden resize-y placeholder:text-text-mute"
            spellCheck={false}
            placeholder="SELECT ..."
          />
          <div className="flex items-center gap-2 mt-2">
            <div className="text-xs text-text-mute">⌘/Ctrl+Enter to run</div>
            <button onClick={() => setSql(SAMPLE)} className="btn-ghost ml-auto py-1.5">Sample</button>
            <button onClick={run} disabled={running} className="btn-primary py-1.5">
              <Icon name="terminal" className="w-4 h-4" />
              {running ? 'Running…' : 'Run'}
            </button>
          </div>
        </div>

        {error && (
          <div className="rounded-[14px] border border-red-500/40 bg-red-500/10 p-4 text-sm text-red-300 font-mono whitespace-pre-wrap">
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
              >
                <Icon name="download" className="w-3.5 h-3.5" />
                CSV
              </button>
            </div>
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
                onClick={() => { setSql(h); taRef.current?.focus() }}
                className="w-full text-left text-xs font-mono text-text-dim hover:text-text break-all"
                title={h}
              >
                {h.length > 60 ? h.slice(0, 60) + '…' : h}
              </button>
            </li>
          ))}
        </ul>
      </aside>
    </div>
  )
}
