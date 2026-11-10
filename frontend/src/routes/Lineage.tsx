import { useMemo, useState, type FormEvent } from 'react'
import { Link, useSearchParams } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { api, errorMessage, type LineageEdgeView, type LineageJobView, type LineageNodeView } from '../api'

// The diagram paints with the theme tokens (index.css), so it follows the four themes.
const ACCENT = 'rgb(var(--c-accent))'
const MUTED = 'rgb(var(--c-text-mute))'

export default function Lineage() {
  const [params, setParams] = useSearchParams()
  const catalog = params.get('catalog') ?? ''
  const schema = params.get('schema') ?? ''
  const table = params.get('table') ?? ''
  const depth = Number(params.get('depth') ?? '2') || 2
  const rawDays = Number(params.get('days') ?? '30')
  const days = [7, 30, 90].includes(rawDays) ? rawDays : 30
  const hasSelection = Boolean(catalog && schema && table)
  const catalogHref = hasSelection
    ? `/catalog?${new URLSearchParams({ catalog, ns: schema, tbl: table }).toString()}`
    : ''

  const { data, isLoading, error } = useQuery({
    queryKey: ['lineage', catalog, schema, table, depth, days],
    queryFn: () => api.lineage({ catalog, schema, table, depth, days }),
    enabled: hasSelection,
    retry: false,
  })

  const pick = (c: string, s: string, t: string) =>
    setParams({ ...Object.fromEntries(params), catalog: c, schema: s, table: t, depth: String(depth) })

  return (
    <div className="max-w-6xl space-y-6">
      <header>
        <div className="flex items-start justify-between gap-4">
          <h1 className="text-2xl font-bold tracking-tight">Lineage</h1>
          <div className="flex items-center gap-3">
            <label className="flex items-center gap-2 text-xs text-text-mute">
              Window
              <select
                className="input py-1 pr-7 text-sm"
                aria-label="Lineage window"
                value={days}
                onChange={e => {
                  const next = new URLSearchParams(params)
                  next.set('days', String(Number(e.target.value)))
                  setParams(next)
                }}
              >
                <option value={7}>7 days</option>
                <option value={30}>30 days</option>
                <option value={90}>90 days</option>
              </select>
            </label>
            {catalogHref && (
              <Link
                to={catalogHref}
                className="btn-ghost"
                title="Open this table in the catalog explorer"
              >
                Open in catalog
              </Link>
            )}
          </div>
        </div>
        <p className="text-text-dim text-sm mt-1">
          Table lineage across the lakehouse, drawn from recorded OpenLineage events.
        </p>
      </header>

      <PickTable
        key={`${catalog}.${schema}.${table}`}
        initial={hasSelection ? `${catalog}.${schema}.${table}` : ''}
        onPick={pick}
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
            jobs={data.jobs ?? []}
            depth={depth}
            days={days}
            onPick={pick}
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
  jobs,
  depth,
  days,
  onPick,
}: {
  nodes: LineageNodeView[]
  edges: LineageEdgeView[]
  jobs: LineageJobView[]
  depth: number
  days: number
  onPick: (c: string, s: string, t: string) => void
}) {
  const layout = useMemo(() => layoutNodes(nodes, edges), [nodes, edges])
  const [hovered, setHovered] = useState<string | null>(null)
  const neighbours = useMemo(() => {
    const m = new Map<string, Set<string>>()
    for (const e of edges) {
      if (!m.has(e.from)) m.set(e.from, new Set())
      if (!m.has(e.to)) m.set(e.to, new Set())
      m.get(e.from)!.add(e.to)
      m.get(e.to)!.add(e.from)
    }
    return m
  }, [edges])
  const isNeighbour = (id: string, of: string) => neighbours.get(of)?.has(id) ?? false

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
          const lit = hovered == null || e.from === hovered || e.to === hovered
          const job = jobs.find((j) => j.name === e.jobName)
          const title = job
            ? `${e.jobName} · ${e.eventTime} · ${job.runs} runs, last ${job.lastRun}`
            : `${e.jobName} · ${e.eventTime}`
          return (
            <Edge
              key={i}
              x1={a.x + 220}
              y1={a.y + 32}
              x2={b.x}
              y2={b.y + 32}
              jobName={e.jobName}
              title={title}
              highlighted={hovered != null && lit}
              dimmed={!lit}
            />
          )
        })}

        {nodes.map((n) => {
          const p = layout.pos[n.id]
          if (!p) return null
          const [cat, sch] = n.namespace.split('.')
          const clickable = n.visible && Boolean(cat && sch)
          const lit = hovered == null || n.id === hovered || isNeighbour(n.id, hovered)
          return (
            <Node
              key={n.id}
              x={p.x}
              y={p.y}
              title={n.visible ? n.name : (n.label || 'a dataset you cannot see')}
              sub={n.visible ? n.namespace : ''}
              accent={p.col === 0}
              muted={!n.visible}
              dimmed={!lit}
              onHover={() => setHovered(n.id)}
              onLeave={() => setHovered(null)}
              onClick={clickable ? () => onPick(cat, sch, n.name) : undefined}
            />
          )
        })}
      </svg>
      <p className="text-xs text-text-mute mt-2">Depth {depth} · last {days} days · hover a dataset to trace it · click to focus</p>
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

function Edge({
  x1,
  y1,
  x2,
  y2,
  jobName,
  title,
  highlighted,
  dimmed,
}: {
  x1: number
  y1: number
  x2: number
  y2: number
  jobName: string
  title: string
  highlighted?: boolean
  dimmed?: boolean
}) {
  const mx = (x1 + x2) / 2
  const my = (y1 + y2) / 2
  const w = Math.max(60, jobName.length * 7 + 16)
  return (
    <g>
      <title>{title}</title>
      <path
        d={`M${x1},${y1} C${mx},${y1} ${mx},${y2} ${x2},${y2}`}
        fill="none"
        style={{ stroke: ACCENT }}
        strokeOpacity={dimmed ? '0.15' : '0.55'}
        strokeWidth={highlighted ? '2.6' : '1.6'}
        markerEnd="url(#arr)"
      />
      {!dimmed && (
        <g>
          <rect
            x={mx - w / 2}
            y={my - 10}
            width={w}
            height={20}
            rx={8}
            style={{ fill: 'rgb(var(--c-bg-elev))', stroke: 'rgb(var(--c-border))' }}
          />
          <text
            x={mx}
            y={my}
            textAnchor="middle"
            dominantBaseline="middle"
            fontSize="11"
            style={{ fill: 'rgb(var(--c-text-dim))' }}
          >
            {jobName}
          </text>
        </g>
      )}
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
  dimmed,
  onHover,
  onLeave,
  onClick,
}: {
  x: number
  y: number
  title: string
  sub: string
  accent?: boolean
  muted?: boolean
  dimmed?: boolean
  onHover?: () => void
  onLeave?: () => void
  onClick?: () => void
}) {
  const stroke = muted ? MUTED : accent ? ACCENT : 'rgb(var(--c-border))'
  return (
    <g
      onClick={onClick}
      onMouseEnter={onHover}
      onMouseLeave={onLeave}
      opacity={dimmed ? 0.35 : undefined}
      style={onClick ? { cursor: 'pointer' } : undefined}
    >
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
