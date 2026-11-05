import { useMemo, useState, type FormEvent } from 'react'
import { useSearchParams } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { api, errorMessage, type LineageEdgeView, type LineageNodeView } from '../api'

// The diagram paints with the theme tokens (index.css), so it follows the four themes.
const ACCENT = 'rgb(var(--c-accent))'
const MUTED = 'rgb(var(--c-text-mute))'

export default function Lineage() {
  const [params, setParams] = useSearchParams()
  const catalog = params.get('catalog') ?? ''
  const schema = params.get('schema') ?? ''
  const table = params.get('table') ?? ''
  const depth = Number(params.get('depth') ?? '2') || 2
  const hasSelection = Boolean(catalog && schema && table)

  const { data, isLoading, error } = useQuery({
    queryKey: ['lineage', catalog, schema, table],
    queryFn: () => api.lineage({ catalog, schema, table, depth }),
    enabled: hasSelection,
    retry: false,
  })

  return (
    <div className="max-w-6xl space-y-6">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Lineage</h1>
        <p className="text-text-dim text-sm mt-1">
          Table lineage across the lakehouse, drawn from recorded OpenLineage events.
        </p>
      </header>

      <PickTable
        key={`${catalog}.${schema}.${table}`}
        initial={hasSelection ? `${catalog}.${schema}.${table}` : ''}
        onPick={(c, s, t) => setParams({ catalog: c, schema: s, table: t, depth: String(depth) })}
      />

      {!hasSelection && (
        <div className="card p-6">
          <p className="text-sm text-text-dim">
            Choose a table to see where its data comes from and who reads it.
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
              No lineage captured for this table yet. Runs that report OpenLineage events will appear here.
            </p>
          </div>
        ) : (
          <Graph
            nodes={data.nodes}
            edges={data.edges}
            depth={depth}
            onPick={(c, s, t) => setParams({ catalog: c, schema: s, table: t, depth: String(depth) })}
          />
        )
      )}

      <div className="grid md:grid-cols-3 gap-4">
        <Fact label="Capture" value="OpenLineage events" hint="emitted by engines and pipelines" />
        <Fact label="Store" value="Nova" hint="table edges in the platform's own database" />
        <Fact label="Scope" value="Visible catalogs" hint="other datasets appear unnamed" />
      </div>
    </div>
  )
}

function PickTable({ initial, onPick }: { initial: string; onPick: (c: string, s: string, t: string) => void }) {
  const [value, setValue] = useState(initial)
  const [msg, setMsg] = useState('')

  function submit(e: FormEvent) {
    e.preventDefault()
    const parts = value.trim().split('.')
    if (parts.length !== 3 || parts.some((p) => p.length === 0)) {
      setMsg('Use catalog.schema.table, for example warehouse.sales.orders')
      return
    }
    setMsg('')
    onPick(parts[0], parts[1], parts[2])
  }

  return (
    <form className="card p-4 flex flex-wrap items-end gap-3" onSubmit={submit}>
      <label className="flex flex-col gap-1 text-sm">
        <span className="text-text-dim">Table</span>
        <input
          className="input w-80 max-w-full"
          placeholder="catalog.schema.table"
          value={value}
          onChange={(e) => setValue(e.target.value)}
        />
      </label>
      <button type="submit" className="btn btn-primary">Show lineage</button>
      {msg && <p className="text-sm w-full" style={{ color: 'rgb(var(--c-danger, 220 80 80))' }}>{msg}</p>}
    </form>
  )
}

function Graph({
  nodes,
  edges,
  depth,
  onPick,
}: {
  nodes: LineageNodeView[]
  edges: LineageEdgeView[]
  depth: number
  onPick: (c: string, s: string, t: string) => void
}) {
  const layout = useMemo(() => layoutNodes(nodes, edges), [nodes, edges])

  return (
    <div className="card p-6 overflow-auto">
      <svg viewBox={`0 0 ${layout.width} ${layout.height}`} className="w-full min-w-[720px]" role="img" aria-label="Lineage graph">
        <defs>
          <marker id="arr" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <path d="M0,0 L8,4 L0,8 Z" style={{ fill: ACCENT }} />
          </marker>
          <linearGradient id="node" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" style={{ stopColor: 'rgb(var(--c-bg-soft))' }} />
            <stop offset="100%" style={{ stopColor: 'rgb(var(--c-bg-elev))' }} />
          </linearGradient>
        </defs>

        {edges.map((e, i) => {
          const a = layout.pos[e.from]
          const b = layout.pos[e.to]
          if (!a || !b) return null
          return (
            <Edge key={i} x1={a.x + 220} y1={a.y + 32} x2={b.x} y2={b.y + 32} title={`${e.jobName} · ${e.eventTime}`} />
          )
        })}

        {nodes.map((n) => {
          const p = layout.pos[n.id]
          if (!p) return null
          const [cat, sch] = n.namespace.split('.')
          const clickable = n.visible && Boolean(cat && sch)
          return (
            <Node
              key={n.id}
              x={p.x}
              y={p.y}
              title={n.visible ? n.name : (n.label || 'a dataset you cannot see')}
              sub={n.visible ? n.namespace : ''}
              accent={p.col === 0}
              muted={!n.visible}
              onClick={clickable ? () => onPick(cat, sch, n.name) : undefined}
            />
          )
        })}
      </svg>
      <p className="text-xs text-text-mute mt-2">Depth {depth} · click a dataset to focus it.</p>
    </div>
  )
}

function layoutNodes(nodes: LineageNodeView[], edges: LineageEdgeView[]) {
  const start = nodes[0]?.id
  const dist = new Map<string, number>()
  if (start) dist.set(start, 0)
  const adj = new Map<string, string[]>()
  for (const e of edges) {
    if (!adj.has(e.from)) adj.set(e.from, [])
    if (!adj.has(e.to)) adj.set(e.to, [])
    adj.get(e.from)!.push(e.to)
    adj.get(e.to)!.push(e.from)
  }
  const q = start ? [start] : []
  for (let i = 0; i < q.length; i++) {
    const cur = q[i]
    const d = dist.get(cur) ?? 0
    for (const nb of adj.get(cur) ?? []) {
      if (!dist.has(nb)) {
        dist.set(nb, d + 1)
        q.push(nb)
      }
    }
  }
  for (const n of nodes) if (!dist.has(n.id)) dist.set(n.id, 99)

  const cols = new Map<number, string[]>()
  for (const n of nodes) {
    const d = dist.get(n.id) ?? 99
    if (!cols.has(d)) cols.set(d, [])
    cols.get(d)!.push(n.id)
  }
  const pos: Record<string, { x: number; y: number; col: number }> = {}
  const colW = 300
  const rowH = 100
  let maxCol = 0
  let maxRow = 0
  for (const [d, ids] of cols.entries()) {
    maxCol = Math.max(maxCol, d)
    ids.forEach((id, r) => {
      maxRow = Math.max(maxRow, r)
      pos[id] = { x: 40 + d * colW, y: 40 + r * rowH, col: d }
    })
  }
  return { pos, width: Math.max(720, 80 + (maxCol + 1) * colW), height: Math.max(160, 80 + (maxRow + 1) * rowH) }
}

function Edge({ x1, y1, x2, y2, title }: { x1: number; y1: number; x2: number; y2: number; title: string }) {
  const mx = (x1 + x2) / 2
  return (
    <g>
      <title>{title}</title>
      <path
        d={`M${x1},${y1} C${mx},${y1} ${mx},${y2} ${x2},${y2}`}
        fill="none"
        style={{ stroke: ACCENT }}
        strokeOpacity="0.55"
        strokeWidth="1.6"
        markerEnd="url(#arr)"
      />
    </g>
  )
}

function Node({
  x,
  y,
  title,
  sub,
  accent,
  muted,
  onClick,
}: {
  x: number
  y: number
  title: string
  sub: string
  accent?: boolean
  muted?: boolean
  onClick?: () => void
}) {
  const stroke = muted ? MUTED : accent ? ACCENT : 'rgb(var(--c-border))'
  return (
    <g onClick={onClick} style={onClick ? { cursor: 'pointer' } : undefined}>
      <rect
        x={x}
        y={y}
        width={220}
        height={64}
        rx={12}
        fill="url(#node)"
        style={{ stroke }}
        strokeWidth={accent ? 1.6 : 1}
        strokeDasharray={muted ? '4 4' : undefined}
      />
      <text x={x + 14} y={y + 26} style={{ fill: muted ? MUTED : 'rgb(var(--c-text))' }} fontSize="13" fontWeight="600">{title}</text>
      {sub && <text x={x + 14} y={y + 46} style={{ fill: MUTED }} fontSize="11">{sub}</text>}
    </g>
  )
}

function Fact({ label, value, hint }: { label: string; value: string; hint: string }) {
  return (
    <div className="card p-4">
      <div className="label">{label}</div>
      <div className="text-lg font-semibold mt-1">{value}</div>
      <div className="text-xs text-text-mute">{hint}</div>
    </div>
  )
}
