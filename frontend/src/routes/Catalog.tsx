import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useNavigate } from 'react-router-dom'
import { api } from '../api'

export default function Catalog() {
  const [selected, setSelected] = useState<{ ns: string; tbl: string } | null>(null)

  const namespaces = useQuery({
    queryKey: ['catalog', 'namespaces'],
    queryFn: api.namespaces,
  })

  return (
    <div className="grid grid-cols-[280px_1fr] gap-6 h-full">
      <aside className="rounded-[14px] border border-border bg-bg-elev/60 p-4 overflow-auto">
        <h2 className="text-sm uppercase tracking-widest text-text-mute mb-3">Namespaces</h2>
        {namespaces.isLoading && <div className="text-text-dim">loading…</div>}
        {namespaces.isError && <div className="text-red-400 text-sm">{String(namespaces.error)}</div>}
        <ul className="space-y-2">
          {namespaces.data?.namespaces.map(parts => {
            const ns = parts.join('.')
            return <NamespaceItem key={ns} ns={ns} onSelect={setSelected} selected={selected} />
          })}
        </ul>
      </aside>

      <section className="rounded-[14px] border border-border bg-bg-elev/60 p-6 overflow-auto">
        {selected ? (
          <TableDetail ns={selected.ns} tbl={selected.tbl} />
        ) : (
          <div className="text-text-dim">Select a table on the left.</div>
        )}
      </section>
    </div>
  )
}

function NamespaceItem({
  ns,
  onSelect,
  selected,
}: {
  ns: string
  onSelect: (sel: { ns: string; tbl: string }) => void
  selected: { ns: string; tbl: string } | null
}) {
  const [open, setOpen] = useState(true)
  const tables = useQuery({
    queryKey: ['catalog', 'tables', ns],
    queryFn: () => api.tables(ns),
    enabled: open,
  })
  return (
    <li>
      <button
        onClick={() => setOpen(o => !o)}
        className="w-full text-left text-text-dim hover:text-text font-medium"
      >
        {open ? '▾' : '▸'} {ns}
      </button>
      {open && (
        <ul className="ml-4 mt-1 space-y-1">
          {tables.isLoading && <li className="text-text-mute text-sm">loading…</li>}
          {tables.data?.identifiers.map(id => {
            const sel = selected?.ns === ns && selected?.tbl === id.name
            return (
              <li key={id.name}>
                <button
                  onClick={() => onSelect({ ns, tbl: id.name })}
                  className={[
                    'w-full text-left px-2 py-1 rounded-[10px] text-sm',
                    sel
                      ? 'bg-accent/15 text-text border border-accent/30'
                      : 'text-text-dim hover:text-text hover:bg-bg-soft',
                  ].join(' ')}
                >
                  {id.name}
                </button>
              </li>
            )
          })}
        </ul>
      )}
    </li>
  )
}

function TableDetail({ ns, tbl }: { ns: string; tbl: string }) {
  const navigate = useNavigate()
  const meta = useQuery({
    queryKey: ['catalog', 'table', ns, tbl],
    queryFn: () => api.tableMeta(ns, tbl),
  })

  if (meta.isLoading) return <div className="text-text-dim">loading…</div>
  if (meta.isError)   return <div className="text-red-400">{String(meta.error)}</div>

  const m = meta.data!.metadata
  const schemaId = m['current-schema-id'] ?? m.schemas[0]?.['schema-id'] ?? 0
  const schema = m.schemas.find(s => s['schema-id'] === schemaId) ?? m.schemas[0]
  const partSpec = m['partition-specs']?.[0]

  return (
    <div className="space-y-6">
      <header className="flex items-center justify-between">
        <div>
          <div className="text-xs uppercase tracking-widest text-text-mute">{ns}</div>
          <h2 className="text-2xl font-bold">{tbl}</h2>
        </div>
        <button
          onClick={() => navigate(`/query?sql=${encodeURIComponent(`SELECT * FROM warehouse.${ns}.${tbl} LIMIT 100`)}`)}
          className="px-4 py-2 rounded-[10px] bg-accent text-white text-sm font-medium hover:opacity-90"
        >
          Query this table →
        </button>
      </header>

      <section>
        <h3 className="text-sm uppercase tracking-widest text-text-mute mb-2">Schema</h3>
        <table className="w-full text-sm font-mono">
          <thead className="text-text-mute">
            <tr><th className="text-left py-1">id</th><th className="text-left py-1">name</th><th className="text-left py-1">type</th><th className="text-left py-1">req</th></tr>
          </thead>
          <tbody>
            {schema.fields.map(f => (
              <tr key={f.id} className="border-t border-border">
                <td className="py-1 text-text-dim">{f.id}</td>
                <td className="py-1">{f.name}</td>
                <td className="py-1 text-text-dim">{typeof f.type === 'string' ? f.type : JSON.stringify(f.type)}</td>
                <td className="py-1 text-text-dim">{f.required ? '✓' : ''}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>

      {partSpec && partSpec.fields.length > 0 && (
        <section>
          <h3 className="text-sm uppercase tracking-widest text-text-mute mb-2">Partition spec</h3>
          <ul className="text-sm font-mono space-y-1">
            {partSpec.fields.map(f => (
              <li key={f.name}>
                <span className="text-accent">{f.transform}</span>
                <span className="text-text-dim">(source_id={f['source-id']})</span> → {f.name}
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  )
}
