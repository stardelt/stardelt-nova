import { useEffect, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useSearchParams } from 'react-router-dom'
import { api, formatCell, formatTime, type QueryResult, type TableMetadata } from '../api'
import { sqlTableRef, rowsLabel } from '../format'
import { isAdmin } from '../roles'

type Sel = { ns: string; tbl: string }

export default function Catalog() {
  const [params] = useSearchParams()
  const [selected, setSelected] = useState<Sel | null>(null)
  const [filter, setFilter] = useState(params.get('q') ?? '')

  useEffect(() => {
    const ns = params.get('ns')
    const tbl = params.get('tbl')
    if (ns && tbl) setSelected({ ns, tbl })
  }, [params])

  useEffect(() => {
    const q = params.get('q')
    if (q) setFilter(q)
  }, [params])

  const namespaces = useQuery({ queryKey: ['catalog', 'namespaces'], queryFn: api.namespaces })

  return (
    <div className="grid grid-cols-1 lg:grid-cols-[300px_minmax(0,1fr)] gap-5 h-full min-h-0">
      <aside className="card p-3 flex flex-col min-h-0">
        <div className="px-1 pb-2">
          <div className="label mb-1.5">Catalog</div>
          <input
            value={filter}
            onChange={e => setFilter(e.target.value)}
            placeholder="Filter tables…"
            aria-label="Filter tables"
            className="input py-1.5"
          />
        </div>
        <div className="overflow-auto flex-1">
          {namespaces.isLoading && <div className="text-text-mute text-sm p-2">loading…</div>}
          {namespaces.isError && <div className="text-bad text-sm p-2">{String(namespaces.error)}</div>}
          <ul className="space-y-1">
            {namespaces.data?.namespaces.map(parts => {
              const ns = parts.join('.')
              return <NamespaceItem key={ns} ns={ns} filter={filter} onSelect={setSelected} selected={selected} />
            })}
          </ul>
        </div>
      </aside>

      <section className="min-w-0 min-h-0 overflow-auto">
        {selected ? (
          <TableDetail ns={selected.ns} tbl={selected.tbl} />
        ) : (
          <div className="card p-8 h-full grid place-items-center text-center">
            <div>
              <div className="text-text-dim">Select a table to inspect it.</div>
              <div className="text-text-mute text-sm mt-1">Or load a file to create one.</div>
              <Link to="/data" className="btn-ghost mt-4">Load data</Link>
            </div>
          </div>
        )}
      </section>
    </div>
  )
}

function NamespaceItem({ ns, filter, onSelect, selected }: {
  ns: string
  filter: string
  onSelect: (sel: Sel) => void
  selected: Sel | null
}) {
  const [open, setOpen] = useState(true)
  const tables = useQuery({ queryKey: ['catalog', 'tables', ns], queryFn: () => api.tables(ns), enabled: open })

  const matches = (name: string) => {
    if (!filter.trim()) return true
    return `${ns}.${name}`.toLowerCase().includes(filter.toLowerCase())
  }
  const visible = (tables.data?.identifiers ?? []).filter(id => matches(id.name))

  if (filter.trim() && visible.length === 0) return null

  return (
    <li>
      <button onClick={() => setOpen(o => !o)} className="w-full flex items-center gap-2 text-left px-2 py-1.5 rounded-[8px] hover:bg-bg-soft/40">
        <span className="text-text-mute text-xs">{open ? '▾' : '▸'}</span>
        <span className="text-sm font-medium">{ns}</span>
        <span className="ml-auto text-[11px] text-text-mute">{visible.length}</span>
      </button>
      {open && (
        <ul className="ml-3 mt-0.5 space-y-0.5">
          {tables.isLoading && <li className="text-text-mute text-xs px-2 py-1">loading…</li>}
          {tables.isError && !tables.data && <li className="text-bad text-xs px-2 py-1 break-words">{String(tables.error)}</li>}
          {visible.map(id => {
            const sel = selected?.ns === ns && selected?.tbl === id.name
            return (
              <li key={id.name}>
                <button
                  onClick={() => onSelect({ ns, tbl: id.name })}
                  aria-current={sel ? 'true' : undefined}
                  className={[
                    'w-full text-left px-2 py-1.5 rounded-[8px] text-sm flex items-center gap-2',
                    sel ? 'bg-accent/15 text-text border border-accent/30' : 'text-text-dim hover:text-text hover:bg-bg-soft/40 border border-transparent',
                  ].join(' ')}
                >
                  <span className="text-text-mute">◇</span>
                  <span className="truncate">{id.name}</span>
                </button>
              </li>
            )
          })}
        </ul>
      )}
    </li>
  )
}

function TableDetail({ ns, tbl }: Sel) {
  const [tab, setTab] = useState<'preview' | 'columns' | 'details' | 'history'>('preview')
  const meta = useQuery({ queryKey: ['catalog', 'table', ns, tbl], queryFn: () => api.tableMeta(ns, tbl) })
  const preview = useQuery({
    queryKey: ['preview', ns, tbl],
    queryFn: () => api.query(`SELECT * FROM ${sqlTableRef(ns, tbl)} LIMIT 50`),
    enabled: tab === 'preview',
  })

  const m = meta.data?.metadata
  const schemaId = m?.['current-schema-id'] ?? m?.schemas[0]?.['schema-id'] ?? 0
  const schema = m?.schemas.find(s => s['schema-id'] === schemaId) ?? m?.schemas[0]
  const partSpec = m?.['partition-specs']?.[0]

  return (
    <div className="space-y-4">
      <header className="flex items-start justify-between gap-4 flex-wrap">
        <div>
          <div className="text-[11px] uppercase tracking-widest text-text-mute">{ns}</div>
          <h1 className="text-2xl font-bold tracking-tight font-mono">{tbl}</h1>
          {m && (
            <div className="text-xs text-text-mute mt-1 font-mono truncate max-w-[620px]">{m.location}</div>
          )}
        </div>
        <div className="flex gap-2">
          <Link to={`/query?sql=${encodeURIComponent(`SELECT * FROM ${sqlTableRef(ns, tbl)} LIMIT 100`)}`} className="btn-primary">
            Query table
          </Link>
          <Link to="/data" className="btn-ghost">Load data</Link>
        </div>
      </header>

      <div className="flex gap-2">
        {(['preview', 'columns', 'details', 'history'] as const).map(t => (
          <button
            key={t}
            onClick={() => setTab(t)}
            aria-pressed={tab === t}
            className={[
              'px-3 py-1.5 rounded-[8px] text-sm border capitalize',
              tab === t
                ? 'bg-accent/15 border-accent/30 text-text'
                : 'border-border text-text-dim hover:text-text',
            ].join(' ')}
          >
            {t}
          </button>
        ))}
      </div>

      {meta.isError && <div className="text-bad text-sm">{String(meta.error)}</div>}

      {tab === 'preview' && <Preview q={preview} />}

      {tab === 'columns' && (
        <div className="card overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="bg-bg-soft/50">
              <tr>
                <th className="th px-4 py-2.5">#</th>
                <th className="th px-4 py-2.5">Column</th>
                <th className="th px-4 py-2.5">Type</th>
                <th className="th px-4 py-2.5">Required</th>
              </tr>
            </thead>
            <tbody>
              {schema?.fields.map(f => (
                <tr key={f.id} className="border-t border-border/70">
                  <td className="td px-4 text-text-mute font-mono">{f.id}</td>
                  <td className="td px-4 font-mono">{f.name}</td>
                  <td className="td px-4 text-text-dim font-mono">{typeof f.type === 'string' ? f.type : JSON.stringify(f.type)}</td>
                  <td className="td px-4 text-text-mute">{f.required ? 'yes' : '—'}</td>
                </tr>
              ))}
              {!schema && <tr><td className="px-4 py-5 text-text-mute" colSpan={4}>loading…</td></tr>}
            </tbody>
          </table>
        </div>
      )}

      {tab === 'details' && meta.data && <Details meta={meta.data} partition={partSpec} />}

      {tab === 'history' && <HistoryTab ns={ns} tbl={tbl} />}
    </div>
  )
}

function Preview({ q }: { q: { data?: QueryResult; isLoading: boolean; error: unknown } }) {
  if (q.isLoading) return <div className="card p-6 text-text-mute text-sm">loading preview…</div>
  if (q.error) return <div className="card p-6 text-bad text-sm">{String(q.error)}</div>
  if (!q.data) return null
  return (
    <div className="card overflow-hidden">
      <div className="px-4 py-2 text-xs text-text-mute border-b border-border flex justify-between">
        <span>{rowsLabel(q.data.row_count)} · preview</span>
        <span className="font-mono">{q.data.query_id}</span>
      </div>
      <div className="overflow-auto max-h-[60vh]">
        <table className="w-full text-sm font-mono">
          <thead className="bg-bg-soft/60 sticky top-0">
            <tr>{q.data.columns.map((c, i) => <th key={i} className="th px-4 py-2">{c.name}</th>)}</tr>
          </thead>
          <tbody>
            {q.data.rows.map((row, i) => (
              <tr key={i} className="border-t border-border/60">
                {row.map((cell, j) => <td key={j} className="td px-4 text-text-dim">{formatCell(cell)}</td>)}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  )
}

function HistoryTab({ ns, tbl }: Sel) {
  const qc = useQueryClient()
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const history = useQuery({
    queryKey: ['catalog', 'history', ns, tbl],
    queryFn: () => api.tableHistory(ns, tbl),
  })
  const [previewId, setPreviewId] = useState<string | null>(null)
  const [rollbackTarget, setRollbackTarget] = useState<string | null>(null)
  const [confirmText, setConfirmText] = useState('')
  const [rollbackOk, setRollbackOk] = useState<string | null>(null)

  const preview = useQuery({
    queryKey: ['history-preview', ns, tbl, previewId],
    queryFn: () =>
      api.query(`SELECT * FROM ${sqlTableRef(ns, tbl)} FOR VERSION AS OF ${previewId} LIMIT 100`),
    enabled: previewId !== null,
  })

  const rollback = useMutation({
    mutationFn: (snapshotId: string) => api.rollback(ns, tbl, snapshotId),
    onSuccess: (_r, snapshotId) => {
      setRollbackOk(`Rolled back to snapshot ${snapshotId}.`)
      setRollbackTarget(null)
      setConfirmText('')
      qc.invalidateQueries({ queryKey: ['catalog', 'history', ns, tbl] })
      qc.invalidateQueries({ queryKey: ['preview', ns, tbl] })
      if (previewId !== null) qc.invalidateQueries({ queryKey: ['history-preview', ns, tbl, previewId] })
    },
  })

  const admin = isAdmin(me.data)
  const cols = history.data?.columns ?? []
  const idx = (name: string) => cols.findIndex(c => c.name === name)
  const iTime = idx('committed_at')
  const iSnap = idx('snapshot_id')
  const iOp = idx('operation')
  const iSummary = idx('summary')

  if (history.isLoading) return <div className="card p-6 text-text-mute text-sm">loading history…</div>
  if (history.isError) return <div className="card p-6 text-bad text-sm">{String(history.error)}</div>
  const rows = history.data?.rows ?? []
  if (rows.length === 0) {
    return (
      <div className="card p-6 text-text-mute text-sm">
        No snapshots recorded. {me.data?.demo ? 'Demo mode has no table history.' : 'The table has no snapshots yet.'}
      </div>
    )
  }

  const snapIdOf = (row: unknown[]): string | null => {
    if (iSnap < 0) return null
    const v = row[iSnap]
    // The backend sends ids as strings to keep all 64 bits through JSON.parse;
    // an old backend may still send a safe number. Anything else is unusable.
    if (typeof v === 'string' && /^-?\d+$/.test(v)) return v
    if (typeof v === 'number' && Number.isSafeInteger(v)) return String(v)
    return null
  }

  const summaryText = (row: unknown[]): string => {
    if (iSummary < 0) return '—'
    const v = row[iSummary] as unknown
    if (v === null || v === undefined) return '—'
    if (typeof v === 'object') {
      const o = v as Record<string, unknown>
      // Iceberg snapshot summaries carry row/file counts under various keys;
      // surface the familiar ones when present, else the raw map.
      const picks = ['total-records', 'added-records', 'deleted-records', 'added-data-files', 'deleted-data-files']
        .filter(k => o[k] !== undefined)
        .map(k => `${k}=${String(o[k])}`)
      if (picks.length > 0) return picks.join(' · ')
      return JSON.stringify(o)
    }
    return String(v)
  }

  return (
    <div className="space-y-4">
      <div className="card overflow-x-auto">
        <table className="w-full text-sm">
          <thead className="bg-bg-soft/50">
            <tr>
              <th className="th px-4 py-2.5">Time</th>
              <th className="th px-4 py-2.5">Operation</th>
              <th className="th px-4 py-2.5">Snapshot</th>
              <th className="th px-4 py-2.5">Summary</th>
              <th className="th px-4 py-2.5"><span className="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row, i) => {
              const id = snapIdOf(row)
              return (
                <tr key={i} className="border-t border-border/70">
                  <td className="td px-4 font-mono whitespace-nowrap">{iTime >= 0 ? formatCell(row[iTime]) : '—'}</td>
                  <td className="td px-4 font-mono">{iOp >= 0 ? formatCell(row[iOp]) : '—'}</td>
                  <td className="td px-4 font-mono text-text-mute">{id !== null ? String(id) : '—'}</td>
                  <td className="td px-4 font-mono text-text-dim break-all">{summaryText(row)}</td>
                  <td className="td px-4 whitespace-nowrap text-right">
                    {id !== null && (
                      <button
                        onClick={() => setPreviewId(id)}
                        aria-pressed={previewId === id}
                        className="btn-ghost py-1 px-2 text-xs"
                      >
                        Preview
                      </button>
                    )}
                    {admin && id !== null && (
                      <button
                        onClick={() => { setRollbackTarget(id); setConfirmText(''); setRollbackOk(null); rollback.reset() }}
                        className="btn-ghost py-1 px-2 text-xs text-bad ml-2"
                      >
                        Roll back…
                      </button>
                    )}
                  </td>
                </tr>
              )
            })}
          </tbody>
        </table>
      </div>

      {rollbackTarget !== null && (
        <div className="card p-5 border-red-500/40" role="dialog" aria-label="Confirm rollback">
          <div className="text-sm font-semibold">Roll back {ns}.{tbl} to snapshot {rollbackTarget}?</div>
          <p className="text-sm text-text-dim mt-1">
            This moves the current pointer to that snapshot. No snapshots or files are deleted,
            and the rollback itself appears in the history.
          </p>
          <label className="block mt-3">
            <span className="label block mb-1.5">Type <span className="font-mono">{tbl}</span> to confirm</span>
            <input
              value={confirmText}
              onChange={e => setConfirmText(e.target.value)}
              className="input"
              aria-label={`Type ${tbl} to confirm rollback`}
            />
          </label>
          {rollback.isError && (
            <div className="rounded-[10px] border border-red-500/40 bg-red-500/10 p-3 mt-3 text-sm text-bad font-mono whitespace-pre-wrap break-words">
              {String(rollback.error)}
            </div>
          )}
          <div className="flex gap-2 mt-4">
            <button
              disabled={confirmText !== tbl || rollback.isPending}
              onClick={() => rollback.mutate(rollbackTarget)}
              className="btn-primary disabled:opacity-40"
            >
              {rollback.isPending ? 'Rolling back…' : 'Roll back to this version'}
            </button>
            <button onClick={() => { setRollbackTarget(null); setConfirmText(''); rollback.reset() }} className="btn-ghost">
              Cancel
            </button>
          </div>
        </div>
      )}
      {rollbackOk && <div className="rounded-[10px] border border-emerald-400/30 bg-emerald-400/10 p-3 text-sm text-ok">{rollbackOk}</div>}
      {rollback.isError && rollbackTarget === null && (
        <div className="rounded-[10px] border border-red-500/40 bg-red-500/10 p-3 text-sm text-bad font-mono whitespace-pre-wrap break-words">
          {String(rollback.error)}
        </div>
      )}

      {previewId !== null && (
        <div className="space-y-2">
          <div className="text-xs text-text-mute font-mono">
            Time travel: <span className="text-text-dim">SELECT * FROM {sqlTableRef(ns, tbl)} FOR VERSION AS OF {previewId} LIMIT 100</span>
            <button onClick={() => setPreviewId(null)} className="btn-ghost py-0.5 px-2 text-xs ml-3">Close</button>
          </div>
          <Preview q={preview} />
        </div>
      )}
    </div>
  )
}

function Details({ meta, partition }: {
  meta: TableMetadata
  partition?: { fields: Array<{ name: string; transform: string; 'source-id': number }> }
}) {
  const m = meta.metadata
  return (
    <div className="grid md:grid-cols-2 gap-4">
      <div className="card p-5">
        <div className="label mb-3">Table</div>
        <Row k="UUID" v={m['table-uuid']} mono />
        <Row k="Location" v={m.location} mono />
        <Row k="Updated" v={formatTime(m['last-updated-ms'])} />
        <Row k="Metadata" v={meta['metadata-location']} mono />
      </div>
      <div className="card p-5">
        <div className="label mb-3">Partitioning</div>
        {partition && partition.fields.length > 0 ? (
          <ul className="text-sm font-mono space-y-1">
            {partition.fields.map(f => (
              <li key={f.name}>
                <span className="text-accent">{f.transform}</span>
                <span className="text-text-mute"> → </span>{f.name}
              </li>
            ))}
          </ul>
        ) : (
          <div className="text-sm text-text-mute">Unpartitioned</div>
        )}
        {m.properties && (
          <>
            <div className="label mt-5 mb-2">Properties</div>
            <ul className="text-sm font-mono space-y-1">
              {Object.entries(m.properties).map(([k, v]) => (
                <li key={k}><span className="text-text-dim">{k}</span> = {v}</li>
              ))}
            </ul>
          </>
        )}
      </div>
    </div>
  )
}

function Row({ k, v, mono }: { k: string; v: string; mono?: boolean }) {
  return (
    <div className="py-1.5 border-b border-border/60 last:border-0">
      <div className="text-[11px] uppercase tracking-widest text-text-mute">{k}</div>
      <div className={`text-sm break-all ${mono ? 'font-mono' : ''}`}>{v}</div>
    </div>
  )
}
