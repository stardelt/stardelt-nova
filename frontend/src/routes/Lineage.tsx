import { useEffect, useMemo, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { api, errorMessage, type LineageGraphView } from '../api'
import { formatCount } from '../format'
import LineageGraph from '../components/LineageGraph'
import { JobPill, LineageLists } from '../components/LineageLists'

/**
 * Table lineage (stardelt-nova#704, lineage L6). A table, this table, the tables that feed it
 * and the tables built from it: a React Flow graph on a wide screen and the two lists of
 * #703 on a phone. The page owns the URL parameters and the toolbar; `LineageGraph` draws.
 */

const STEPS = [1, 2, 3] as const
const WINDOWS = [7, 30, 90] as const

/** Below this width the graph is a vertical smear, so the lists answer instead. */
const PHONE = '(max-width: 767px)'

function usePhone(): boolean {
  const [phone, setPhone] = useState(() => {
    try {
      return window.matchMedia(PHONE).matches
    } catch {
      return false
    }
  })
  useEffect(() => {
    let query: MediaQueryList
    try {
      query = window.matchMedia(PHONE)
    } catch {
      return
    }
    const read = () => setPhone(query.matches)
    read()
    query.addEventListener('change', read)
    return () => query.removeEventListener('change', read)
  }, [])
  return phone
}

export default function Lineage() {
  const [params, setParams] = useSearchParams()
  const catalog = params.get('catalog') ?? ''
  const schema = params.get('schema') ?? ''
  const table = params.get('table') ?? ''
  const rawSteps = Number(params.get('depth') ?? '2')
  const depth = (STEPS as readonly number[]).includes(rawSteps) ? rawSteps : 2
  const rawDays = Number(params.get('days') ?? '30')
  const days = (WINDOWS as readonly number[]).includes(rawDays) ? rawDays : 30
  const handmade = params.get('handmade') === '1'
  const hasSelection = Boolean(catalog && schema && table)
  const phone = usePhone()
  const [fitKey, setFitKey] = useState(0)

  const { data, isLoading, error } = useQuery({
    queryKey: ['lineage', catalog, schema, table, depth, days],
    queryFn: () => api.lineage({ catalog, schema, table, depth, days }),
    enabled: hasSelection,
    retry: false,
  })

  // The runs of the caller's pipelines: the graph marks a table with the state of the run
  // that last wrote it. One request for the whole page, and only once a table is picked.
  const runs = useQuery({
    queryKey: ['runs', 'lineage-health'],
    queryFn: () => api.runs({ limit: 200 }),
    enabled: hasSelection,
    staleTime: 60_000,
    retry: false,
  })

  /** `<namespace>/<name>` → the newest run's state, for the writers the graph draws. */
  const health = useMemo(() => {
    const map = new Map<string, { state: string; startedAt: string | null }>()
    for (const r of runs.data?.runs ?? []) {
      const key = `${r.namespace}/${r.name}`
      if (!map.has(key)) map.set(key, { state: r.state, startedAt: r.startedAt })
    }
    return map
  }, [runs.data])

  const setParam = (key: string, value: string | null) => {
    const next = new URLSearchParams(params)
    if (value === null) next.delete(key)
    else next.set(key, value)
    setParams(next)
  }

  const pick = (c: string, s: string, t: string) => {
    const next = new URLSearchParams(params)
    next.set('catalog', c)
    next.set('schema', s)
    next.set('table', t)
    setParams(next)
  }

  // No table picked: open on the table the caller reads most, preferring one that something writes.
  const readTables = useQuery({
    queryKey: ['read-tables'],
    queryFn: () => api.readTables(),
    enabled: !hasSelection,
    staleTime: 60_000,
    retry: false,
  })
  useEffect(() => {
    if (hasSelection) return
    const candidates = (readTables.data?.tables ?? []).filter((t) => t.namespace.includes('.'))
    const first = candidates.find((t) => t.lastWrite) ?? candidates[0]
    if (!first) return
    const dot = first.namespace.indexOf('.')
    const next = new URLSearchParams(params)
    next.set('catalog', first.namespace.slice(0, dot))
    next.set('schema', first.namespace.slice(dot + 1))
    next.set('table', first.name)
    setParams(next, { replace: true })
  }, [hasSelection, readTables.data, params, setParams])

  const graph: LineageGraphView = data ?? {
    nodes: [],
    edges: [],
    jobs: [],
    impact: { downstream: 0, upstream: 0, downstreamJobs: 0 },
  }
  const focus = graph.nodes.find(n => n.level === 0) ?? graph.nodes[0]
  const writer = focus?.writers?.[0]
  const handMadeCount = graph.nodes.filter(n => n.handMade).length
  const tableInUrl = hasSelection ? `${catalog}.${schema}.${table}` : ''

  return (
    <div className="max-w-6xl space-y-4">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Lineage</h1>
        <p className="text-text-dim text-sm mt-1">
          Where a table's data comes from and where it goes, recorded each time a query, notebook or pipeline writes it.
        </p>
      </header>

      <div className="card p-3">
        <div className="flex flex-wrap items-center gap-3">
          <TablePicker value={tableInUrl} onPick={pick} />

          <div
            role="group"
            aria-label="Steps away from this table"
            className="flex overflow-hidden rounded-[10px] border border-border text-sm"
          >
            <span className="bg-bg-elev/60 px-3 py-1.5 text-text-mute">Steps</span>
            {STEPS.map(step => (
              <button
                key={step}
                type="button"
                aria-pressed={depth === step}
                onClick={() => setParam('depth', String(step))}
                className={`w-9 px-0 py-1.5 ${depth === step ? 'bg-accent-solid text-white' : 'bg-bg-elev/60 text-text-dim hover:text-text'}`}
              >
                {step}
              </button>
            ))}
          </div>

          <label className="flex items-center gap-2 text-xs text-text-mute">
            Window
            <select
              className="input py-1 pr-7 text-sm"
              aria-label="Lineage window"
              value={days}
              onChange={e => setParam('days', e.target.value)}
            >
              {WINDOWS.map(d => (
                <option key={d} value={d}>
                  {d} days
                </option>
              ))}
            </select>
          </label>

          <label className="flex items-center gap-2 text-sm text-text-dim">
            <input
              type="checkbox"
              className="accent-accent"
              checked={handmade}
              onChange={e => setParam('handmade', e.target.checked ? '1' : null)}
            />
            Show tables made by hand ({formatCount(handMadeCount)})
          </label>

          <button
            type="button"
            className="btn-ghost ml-auto"
            onClick={() => setFitKey(k => k + 1)}
            disabled={!hasSelection}
          >
            Fit
          </button>
        </div>

        {writer && (
          <div className="mt-3 flex flex-wrap items-center gap-2 text-sm text-text-dim">
            <span>Made by</span>
            <JobPill job={writer} />
            <span className="text-text-mute">·</span>
            <span title={writer.lastRun}>
              {formatCount(writer.runs)} {writer.runs === 1 ? 'run' : 'runs'}
            </span>
          </div>
        )}
      </div>

      {!hasSelection && (
        <div className="card p-6">
          <p className="text-sm text-text-dim">
            Pick a table to see what it is made from and what uses it. You can also open a table
            in the Catalog and choose its Lineage tab.
          </p>
        </div>
      )}

      {hasSelection && isLoading && <p className="text-sm text-text-mute">Loading lineage…</p>}

      {hasSelection && error != null && (
        <p className="text-sm" style={{ color: 'rgb(var(--c-danger, 220 80 80))' }}>
          {errorMessage(error)}
        </p>
      )}

      {hasSelection && !isLoading && error == null && data != null && (
        data.nodes.length <= 1 ? (
          <div className="card p-6">
            <p className="text-sm text-text-dim">
              No lineage for this table yet. It appears after the next query, notebook or pipeline that writes or reads it.
            </p>
          </div>
        ) : phone ? (
          <div className="space-y-3">
            <p className="text-xs text-text-mute">The graph needs a wider screen.</p>
            <LineageLists graph={data} days={days} catalog={catalog} health={health} />
          </div>
        ) : (
          <div className="card p-3">
            <LineageGraph
              graph={data}
              days={days}
              catalog={catalog}
              showHandMade={handmade}
              fitKey={fitKey}
              health={health}
              onPick={pick}
            />
          </div>
        )
      )}
    </div>
  )
}

/**
 * The table a person is looking at (`catalog.schema.table`). A datalist is the list of
 * matches, so the field is also the way back to a table typed by hand.
 */
function TablePicker({ value, onPick }: { value: string; onPick: (c: string, s: string, t: string) => void }) {
  const [typed, setTyped] = useState(value)
  const [query, setQuery] = useState('')
  useEffect(() => setTyped(value), [value])

  // 250 ms after the last keystroke the search runs, so a typed name costs one request.
  useEffect(() => {
    const trimmed = typed.trim()
    if (trimmed === value || trimmed.length < 2) {
      setQuery('')
      return
    }
    const timer = window.setTimeout(() => setQuery(trimmed), 250)
    return () => window.clearTimeout(timer)
  }, [typed, value])

  const search = useQuery({
    queryKey: ['lineage-tables', query],
    queryFn: () => api.searchTables(query, 8),
    enabled: query.length >= 2,
    staleTime: 30_000,
    retry: false,
  })

  const options = useMemo(
    () => (search.data?.matches ?? []).map(m => `${m.catalog}.${m.namespace}.${m.table}`),
    [search.data],
  )

  function choose(next: string) {
    setTyped(next)
    const parts = next.trim().split('.')
    if (parts.length === 3 && parts.every(p => p.length > 0)) onPick(parts[0], parts[1], parts[2])
  }

  return (
    <label className="flex min-w-0 flex-1 basis-full items-center gap-2 text-xs text-text-mute sm:w-[20rem] sm:flex-none sm:basis-auto">
      Table
      <input
        className="input min-w-0 flex-1 py-1 text-sm"
        aria-label="Table"
        placeholder="catalog.schema.table"
        list="lineage-table-options"
        autoComplete="off"
        spellCheck={false}
        value={typed}
        onChange={e => choose(e.target.value)}
      />
      <datalist id="lineage-table-options">
        {options.map(option => (
          <option key={option} value={option} />
        ))}
      </datalist>
    </label>
  )
}
