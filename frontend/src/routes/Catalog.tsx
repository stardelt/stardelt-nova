import { useEffect, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link, useSearchParams } from 'react-router-dom'
import { api, formatCell, formatTime, type QueryResult, type TableMetadata } from '../api'
import { sqlTableRef } from '../format'

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
    <div className="grid grid-cols-[300px_1fr] gap-5 h-full min-h-0">
      <aside className="card p-3 flex flex-col min-h-0">
        <div className="px-1 pb-2">
          <div className="label mb-1.5">Catalog</div>
          <input
            value={filter}
            onChange={e => setFilter(e.target.value)}
            placeholder="Filter tables…"
            className="input py-1.5"
          />
        </div>
        <div className="overflow-auto flex-1">
          {namespaces.isLoading && <div className="text-text-mute text-sm p-2">loading…</div>}
          {namespaces.isError && <div className="text-red-400 text-sm p-2">{String(namespaces.error)}</div>}
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
          {tables.isError && !tables.data && <li className="text-red-400 text-xs px-2 py-1 break-words">{String(tables.error)}</li>}
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
  const [tab, setTab] = useState<'preview' | 'columns' | 'details'>('preview')
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
        {(['preview', 'columns', 'details'] as const).map(t => (
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

      {meta.isError && <div className="text-red-400 text-sm">{String(meta.error)}</div>}

      {tab === 'preview' && <Preview q={preview} />}

      {tab === 'columns' && (
        <div className="card overflow-hidden">
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
    </div>
  )
}

function Preview({ q }: { q: { data?: QueryResult; isLoading: boolean; error: unknown } }) {
  if (q.isLoading) return <div className="card p-6 text-text-mute text-sm">loading preview…</div>
  if (q.error) return <div className="card p-6 text-red-400 text-sm">{String(q.error)}</div>
  if (!q.data) return null
  return (
    <div className="card overflow-hidden">
      <div className="px-4 py-2 text-xs text-text-mute border-b border-border flex justify-between">
        <span>{q.data.row_count} rows · preview</span>
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
