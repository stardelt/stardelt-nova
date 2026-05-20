import { useEffect, useRef, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { api, QueryResult } from '../api'

const HISTORY_KEY = 'stardelt-nova:history'
const HISTORY_LIMIT = 50

function loadHistory(): string[] {
  try { return JSON.parse(localStorage.getItem(HISTORY_KEY) ?? '[]') } catch { return [] }
}
function saveHistory(h: string[]) {
  localStorage.setItem(HISTORY_KEY, JSON.stringify(h.slice(0, HISTORY_LIMIT)))
}

export default function Query() {
  const [params] = useSearchParams()
  const [sql, setSql] = useState<string>(params.get('sql') ?? 'SELECT 1')
  const [running, setRunning] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [result, setResult] = useState<QueryResult | null>(null)
  const [history, setHistory] = useState<string[]>(loadHistory)
  const taRef = useRef<HTMLTextAreaElement>(null)

  // honor a fresh ?sql= when the URL changes
  useEffect(() => {
    const fromUrl = params.get('sql')
    if (fromUrl) setSql(fromUrl)
  }, [params])

  async function run() {
    setRunning(true); setError(null); setResult(null)
    try {
      const r = await api.query(sql)
      setResult(r)
      const next = [sql, ...history.filter(h => h !== sql)].slice(0, HISTORY_LIMIT)
      setHistory(next); saveHistory(next)
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setRunning(false)
    }
  }

  function onKey(e: React.KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
      e.preventDefault()
      run()
    }
  }

  return (
    <div className="grid grid-cols-[1fr_280px] gap-6 h-full">
      <div className="flex flex-col gap-4 min-h-0">
        <div className="rounded-[14px] border border-border bg-bg-elev/60 p-3">
          <textarea
            ref={taRef}
            value={sql}
            onChange={e => setSql(e.target.value)}
            onKeyDown={onKey}
            className="w-full h-32 bg-transparent font-mono text-sm outline-none resize-y"
            spellCheck={false}
            placeholder="SELECT ..."
          />
          <div className="flex items-center justify-between mt-2">
            <div className="text-xs text-text-mute">Cmd/Ctrl+Enter to run</div>
            <button
              onClick={run}
              disabled={running}
              className="px-4 py-2 rounded-[10px] bg-accent text-white text-sm font-medium hover:opacity-90 disabled:opacity-50"
            >
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
          <div className="rounded-[14px] border border-border bg-bg-elev/60 flex-1 overflow-auto">
            <div className="px-4 py-2 text-xs text-text-mute border-b border-border flex justify-between">
              <span>{result.row_count} row{result.row_count === 1 ? '' : 's'}</span>
              <span>{result.query_id}</span>
            </div>
            <table className="w-full text-sm font-mono">
              <thead className="text-text-mute sticky top-0 bg-bg-elev">
                <tr>{result.columns.map(c => <th key={c.name} className="text-left px-4 py-2">{c.name}</th>)}</tr>
              </thead>
              <tbody>
                {result.rows.map((row, i) => (
                  <tr key={i} className="border-t border-border">
                    {row.map((cell, j) => <td key={j} className="px-4 py-1 align-top">{formatCell(cell)}</td>)}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      <aside className="rounded-[14px] border border-border bg-bg-elev/60 p-4 overflow-auto">
        <div className="flex items-center justify-between mb-3">
          <h3 className="text-sm uppercase tracking-widest text-text-mute">History</h3>
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

function formatCell(c: unknown): string {
  if (c === null || c === undefined) return '∅'
  if (typeof c === 'string')          return c
  return JSON.stringify(c)
}
