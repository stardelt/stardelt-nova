import { useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import {
  BaseEdge,
  Controls,
  EdgeLabelRenderer,
  Handle,
  Position,
  ReactFlow,
  ReactFlowProvider,
  getBezierPath,
  useReactFlow,
  type Edge,
  type EdgeProps,
  type EdgeTypes,
  type Node,
  type NodeProps,
  type NodeTypes,
} from '@xyflow/react'
import '@xyflow/react/dist/style.css'
import type { LineageEdgeView, LineageGraphView, LineageNodeView } from '../api'
import { formatCount, relativeTime } from '../format'
import { JobPill } from './LineageLists'

/**
 * The lineage graph (stardelt-nova#704). React Flow draws one column per step away from the
 * focus table — sources left, consumers right — one line per pair of tables, and the jobs'
 * names only on the edges of the node a person is looking at. The hand-drawn SVG it replaces
 * put a label on every curve and placed nodes by undirected distance, so sources and
 * consumers shared columns and hundreds of labels overlapped.
 *
 * `layout` is a pure function of the API answer: the columns are the API's `level`, so no
 * layout library is needed. A column of more than eight tables keeps the seven with the most
 * runs and folds the rest into one `＋ <n> more` row.
 */

/** One job on a line between two tables. */
type EdgeJob = { kind: string; name: string; runs: number }

/** The state of the newest run of a table's writer, from `/api/runs`. */
export type RunHealth = { state: string; startedAt: string | null }

export type TableFlowNode = Node<
  {
    node: LineageNodeView
    /** The table the page is about: level 0, the accent border. */
    focus: boolean
    dimmed: boolean
    /** The newest run of the pipeline that last wrote this table; absent for a person's write. */
    run?: RunHealth
    onSelect?: () => void
  },
  'table'
>

export type MoreFlowNode = Node<
  {
    /** How many tables this column folded away. */
    count: number
    level: number
    dimmed: boolean
    onSelect?: () => void
  },
  'more'
>

export type LineageFlowNode = TableFlowNode | MoreFlowNode

export type LineageEdgeData = {
  /** Every job that moved data along this line. */
  jobs: EdgeJob[]
  runs: number
  handMade: boolean
  dimmed: boolean
  /** Labels appear on hover or while a node is selected, never on their own. */
  showLabel: boolean
}

export type LineageFlowEdge = Edge<LineageEdgeData, 'lineage'>

/** Left-to-right distance between two steps; a node is 220 wide, so 60 px of line shows. */
const COLUMN = 280
/** The height of one row; a node is 64 high, so 32 px sit between two rows. */
const ROW = 96
/** A column folds above this many rows — seven tables and the `more` row fit eight. */
const MAX_ROWS = 8
const ROWS_KEPT = 7

/** A job whose namespace is not `pipeline` is a person or a tool, not a scheduled job. */
const handMadeJob = (namespace: string) => namespace !== 'pipeline'

/** The API answers one edge per job; the graph draws one line per table pair. */
function mergeEdges(edges: LineageEdgeView[]): {
  id: string
  from: string
  to: string
  jobs: EdgeJob[]
  runs: number
  handMade: boolean
}[] {
  const merged = new Map<string, { id: string; from: string; to: string; jobs: EdgeJob[]; runs: number; handMade: boolean }>()
  for (const e of edges) {
    const id = `${e.from}->${e.to}`
    const found = merged.get(id)
    if (!found) {
      merged.set(id, {
        id,
        from: e.from,
        to: e.to,
        jobs: [{ kind: e.jobNamespace, name: e.jobName, runs: e.runs }],
        runs: e.runs,
        handMade: handMadeJob(e.jobNamespace),
      })
      continue
    }
    const same = found.jobs.find(j => j.kind === e.jobNamespace && j.name === e.jobName)
    if (same) same.runs += e.runs
    else found.jobs.push({ kind: e.jobNamespace, name: e.jobName, runs: e.runs })
    found.runs += e.runs
    found.handMade = found.handMade && handMadeJob(e.jobNamespace)
  }
  return [...merged.values()]
}

/**
 * The React Flow nodes and edges of one answer. Columns are the API's `level` (`x = level *
 * COLUMN`); inside a column a table sits next to the neighbours it shares with the column before
 * it, so a line crosses as few other lines as possible. `unlimited` holds the columns a
 * person unfolded by clicking `＋ <n> more`.
 */
export function layout(
  nodes: LineageNodeView[],
  edges: LineageEdgeView[],
  showHandMade: boolean,
  unlimited: ReadonlySet<number> = new Set(),
): { nodes: LineageFlowNode[]; edges: LineageFlowEdge[] } {
  const shown = nodes.filter(n => showHandMade || !n.handMade)
  const kept = new Set(shown.map(n => n.id))
  const merged = mergeEdges(edges.filter(e => kept.has(e.from) && kept.has(e.to)))

  const levelOf = new Map(shown.map(n => [n.id, n.level ?? 0]))
  const levels = [...new Set(levelOf.values())].sort((a, b) => a - b)
  const placedY = new Map<string, number>()
  const flowNodes: LineageFlowNode[] = []

  /** The runs that touch a table: the thicker lines around it, so the busiest stay in view. */
  const runsOf = (id: string) =>
    merged.filter(e => e.from === id || e.to === id).reduce((sum, e) => sum + e.runs, 0)

  for (const level of levels) {
    const column = shown.filter(n => (n.level ?? 0) === level)
    // The average row of the neighbours one column to the left; tables without one follow.
    const anchor = (id: string) => {
      const rows = merged
        .filter(e => e.from === id || e.to === id)
        .filter(e => levelOf.get(e.from === id ? e.to : e.from) === level - 1)
        .map(e => placedY.get(e.from === id ? e.to : e.from))
        .filter((y): y is number => y !== undefined)
      return rows.length === 0 ? Number.MAX_SAFE_INTEGER : rows.reduce((a, b) => a + b, 0) / rows.length
    }
    const ordered = [...column].sort((a, b) => anchor(a.id) - anchor(b.id) || a.name.localeCompare(b.name))

    let ids = ordered.map(n => n.id)
    let folded = 0
    if (ids.length > MAX_ROWS && !unlimited.has(level)) {
      const keep = new Set(
        [...ids].sort((a, b) => runsOf(b) - runsOf(a) || a.localeCompare(b)).slice(0, ROWS_KEPT),
      )
      folded = ids.length - keep.size
      ids = ids.filter(id => keep.has(id))
    }

    const rows = ids.length + (folded > 0 ? 1 : 0)
    const top = ((rows - 1) * ROW) / 2
    ids.forEach((id, index) => {
      const y = index * ROW - top
      placedY.set(id, y)
      const node = column.find(n => n.id === id)!
      flowNodes.push({
        id,
        type: 'table',
        position: { x: level * COLUMN, y },
        data: { node, focus: level === 0, dimmed: false },
        // A non-draggable node carries no `nopan` class of its own, so the pan handler would
        // swallow the click; the class keeps the click and still lets the pane pan.
        className: 'nopan',
        draggable: false,
        focusable: false,
        selectable: false,
        deletable: false,
        ariaLabel: node.visible
          ? `${tableName(node)} · ${level === 0 ? 'this table' : `${Math.abs(level)} steps ${level < 0 ? 'before' : 'after'}`}`
          : 'A table you cannot see',
      })
    })
    if (folded > 0) {
      flowNodes.push({
        id: `more:${level}`,
        type: 'more',
        position: { x: level * COLUMN, y: ids.length * ROW - top },
        data: { count: folded, level, dimmed: false },
        className: 'nopan',
        draggable: false,
        focusable: false,
        selectable: false,
        deletable: false,
        ariaLabel: `＋ ${folded} more tables, show them`,
      })
    }
  }

  const flowEdges: LineageFlowEdge[] = merged.map(e => ({
    id: e.id,
    source: e.from,
    target: e.to,
    type: 'lineage',
    data: { jobs: e.jobs, runs: e.runs, handMade: e.handMade, dimmed: false, showLabel: false },
  }))

  return { nodes: flowNodes, edges: flowEdges }
}

/** `<catalog>.<schema>` namespaces carry both; demo data has the schema alone. */
function splitNamespace(namespace: string): { catalog: string; schema: string } {
  const dot = namespace.indexOf('.')
  return dot > 0
    ? { catalog: namespace.slice(0, dot), schema: namespace.slice(dot + 1) }
    : { catalog: '', schema: namespace }
}

function tableName(n: LineageNodeView): string {
  const { catalog, schema } = splitNamespace(n.namespace)
  return catalog ? `${catalog}.${schema}.${n.name}` : `${schema}.${n.name}`
}

/** `5 min ago`, `3 h ago` — when the write that put the table here happened. */
function lastWrite(n: LineageNodeView): string {
  const ms = Date.parse(n.lastWrite ?? '')
  return Number.isFinite(ms) ? relativeTime(ms) : ''
}

/**
 * The dot on a table: the newest run of the pipeline that wrote it. A state a person cannot
 * act on (`skipped`, an unknown word) is no dot at all, so the table reads as it always did.
 */
function runDot(run: RunHealth | undefined): { paint: string; title: string } | null {
  if (!run) return null
  const ms = Date.parse(run.startedAt ?? '')
  const when = Number.isFinite(ms) ? relativeTime(ms) : ''
  const suffix = when ? ` · ${when}` : ''
  if (run.state === 'success') return { paint: 'bg-ok', title: `Last run: succeeded${suffix}` }
  if (run.state === 'failed' || run.state === 'upstream_failed') {
    return { paint: 'bg-bad', title: `Last run: failed${suffix}` }
  }
  if (run.state === 'running' || run.state === 'queued') {
    return { paint: 'bg-accent animate-pulse', title: 'Running now' }
  }
  return null
}

/* ---------- the nodes ---------- */

function TableView({ data }: NodeProps<TableFlowNode>) {
  const n = data.node
  const { schema } = splitNamespace(n.namespace)
  const written = lastWrite(n)
  const sub = [schema, written].filter(Boolean).join(' · ')
  const interactive = n.visible && Boolean(data.onSelect)
  const dot = runDot(data.run)

  const body = (
    <>
      <div className={`truncate text-sm font-semibold ${n.visible ? 'text-text' : 'text-text-mute'}`}>
        {n.visible ? n.name : 'A table you cannot see'}
      </div>
      {dot && (
        <span
          data-testid="node-health"
          aria-label={dot.title}
          title={dot.title}
          className={`inline-block h-2 w-2 rounded-full ${dot.paint}`}
        />
      )}
      {n.visible && sub && <div className="truncate text-xs text-text-mute">{sub}</div>}
      <Handle
        type="target"
        position={Position.Left}
        isConnectable={false}
        style={{ opacity: 0 }}
      />
      <Handle
        type="source"
        position={Position.Right}
        isConnectable={false}
        style={{ opacity: 0 }}
      />
    </>
  )

  const shape = [
    'flex h-16 w-[220px] flex-col justify-center gap-0.5 rounded-[10px] border bg-bg-elev px-3 py-2',
    data.focus ? 'border-2 border-accent' : 'border-border',
    n.visible ? '' : 'border-dashed',
    data.dimmed ? 'opacity-25' : '',
  ].join(' ')

  if (!interactive) {
    return (
      <div className={shape} title={n.visible ? tableName(n) : 'A table you cannot see'}>
        {body}
      </div>
    )
  }
  return (
    <div
      className={`${shape} cursor-pointer transition-opacity focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent`}
      role="button"
      tabIndex={0}
      title={`${tableName(n)} · press Enter for the details`}
      onKeyDown={e => {
        if (e.key !== 'Enter' && e.key !== ' ') return
        e.preventDefault()
        data.onSelect?.()
      }}
    >
      {body}
    </div>
  )
}

function MoreView({ data }: NodeProps<MoreFlowNode>) {
  return (
    <div
      className={`flex h-16 w-[220px] cursor-pointer items-center justify-center rounded-[10px] border border-dashed border-border bg-bg-soft/60 text-sm text-text-dim transition-opacity hover:text-text focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent ${data.dimmed ? 'opacity-25' : ''}`}
      role="button"
      tabIndex={0}
      title="Show every table of this step"
      onKeyDown={e => {
        if (e.key !== 'Enter' && e.key !== ' ') return
        e.preventDefault()
        data.onSelect?.()
      }}
    >
      ＋ {data.count} more
    </div>
  )
}

/* ---------- the lines ---------- */

function LineEdge({ id, sourceX, sourceY, targetX, targetY, data }: EdgeProps<LineageFlowEdge>) {
  const [path, labelX, labelY] = getBezierPath({
    sourceX,
    sourceY,
    sourcePosition: Position.Right,
    targetX,
    targetY,
    targetPosition: Position.Left,
  })
  const jobCount = data?.runs ?? 1
  const width = 1 + Math.min(2, Math.log10(Math.max(1, jobCount)))
  const muted = data?.handMade === true
  return (
    <>
      <BaseEdge
        id={id}
        path={path}
        markerEnd={muted ? 'url(#lineage-arrow-muted)' : 'url(#lineage-arrow)'}
        style={{
          stroke: muted ? 'rgb(var(--c-text-mute))' : 'rgb(var(--c-accent))',
          strokeWidth: width,
          strokeDasharray: muted ? '5 4' : undefined,
          opacity: data?.dimmed ? 0.25 : 0.85,
        }}
      />
      {data?.showLabel && (
        <EdgeLabelRenderer>
          <div
            className="nodrag nopan pointer-events-none absolute flex flex-col-reverse items-center gap-1"
            style={{
              zIndex: 20,
              // Into the gap between two rows, so a pill never covers a table's name; it
              // grows upward from there when one line carries several jobs.
              transform: `translate(-50%, -100%) translate(${labelX}px, ${labelY - 34}px)`,
            }}
          >
            {data.jobs.map(job => (
              <JobPill key={`${job.kind}|${job.name}`} job={job} />
            ))}
          </div>
        </EdgeLabelRenderer>
      )}
    </>
  )
}

const nodeTypes: NodeTypes = { table: TableView, more: MoreView }
const edgeTypes: EdgeTypes = { lineage: LineEdge }

/* ---------- the graph ---------- */

/** The arrowheads, painted with the theme tokens like the rest of the drawing. */
function Arrowheads() {
  return (
    <svg width="0" height="0" aria-hidden="true" className="absolute">
      <defs>
        <marker
          id="lineage-arrow"
          viewBox="0 0 10 10"
          refX="9"
          refY="5"
          markerWidth="10"
          markerHeight="10"
          markerUnits="userSpaceOnUse"
          orient="auto-start-reverse"
        >
          <path d="M0,0 L10,5 L0,10 Z" style={{ fill: 'rgb(var(--c-accent))' }} />
        </marker>
        <marker
          id="lineage-arrow-muted"
          viewBox="0 0 10 10"
          refX="9"
          refY="5"
          markerWidth="10"
          markerHeight="10"
          markerUnits="userSpaceOnUse"
          orient="auto-start-reverse"
        >
          <path d="M0,0 L10,5 L0,10 Z" style={{ fill: 'rgb(var(--c-text-mute))' }} />
        </marker>
      </defs>
    </svg>
  )
}

export type LineageGraphProps = {
  graph: LineageGraphView
  days: number
  /** The catalog the page was opened with; a node's namespace may not name one. */
  catalog: string
  showHandMade: boolean
  /** Bumped by the page's Fit button: the graph fits itself again. */
  fitKey: number
  /** `<namespace>/<name>` → the newest run of that pipeline, from `/api/runs`. */
  health: Map<string, RunHealth>
  /** Follow a table: the page sets its URL parameters. */
  onPick: (catalog: string, schema: string, table: string) => void
}

export default function LineageGraph(props: LineageGraphProps) {
  return (
    <ReactFlowProvider>
      <Graph {...props} />
    </ReactFlowProvider>
  )
}

function Graph({ graph, days, catalog, showHandMade, fitKey, health, onPick }: LineageGraphProps) {
  const { fitView } = useReactFlow()
  const [hovered, setHovered] = useState<string | null>(null)
  const [selected, setSelected] = useState<string | null>(null)
  const [unlimited, setUnlimited] = useState<ReadonlySet<number>>(() => new Set<number>())
  // Hover wins: a person pointing at a table sees its lines, a selected one keeps them shown.
  const lit = hovered ?? selected

  const laid = useMemo(
    () => layout(graph.nodes, graph.edges, showHandMade, unlimited),
    [graph, showHandMade, unlimited],
  )

  // Rows actually drawn: from the highest to the lowest node, plus one node height.
  const height = useMemo(() => {
    const ys = laid.nodes.map(n => n.position.y)
    const span = ys.length ? Math.max(...ys) - Math.min(...ys) + 64 : 64
    return Math.min(560, Math.max(260, span + 120))
  }, [laid])

  /** The lit table and its neighbours stay as they are; everything else steps back. */
  const near = useMemo(() => {
    if (lit == null) return null
    const ids = new Set([lit])
    for (const e of laid.edges) {
      if (e.source === lit) ids.add(e.target)
      if (e.target === lit) ids.add(e.source)
    }
    return ids
  }, [laid, lit])

  const nodes = useMemo<LineageFlowNode[]>(
    () => {
      // The run that last wrote this table: the newest edge that ends in it. Only pipelines have
      // a run: a SQL-editor or notebook write is a person, not a schedule.
      const newestWriter = (id: string): string | null => {
        let best: LineageEdgeView | undefined
        for (const e of graph.edges) {
          if (e.to !== id || e.jobNamespace !== 'pipeline') continue
          if (!best || (e.eventTime ?? '') > (best.eventTime ?? '')) best = e
        }
        return best ? best.jobName : null
      }

      return laid.nodes.map(n => {
        const dimmed = near != null && !near.has(n.id)
        if (n.type === 'more') {
          const level = n.data.level
          return {
            ...n,
            zIndex: n.id === lit ? 10 : 0,
            data: {
              ...n.data,
              dimmed,
              onSelect: () => setUnlimited(prev => new Set([...prev, level])),
            },
          }
        }
        const id = n.id
        const selectable = n.data.node.visible
        return {
          ...n,
          zIndex: n.id === lit ? 10 : 0,
          data: {
            ...n.data,
            dimmed,
            run: health.get(newestWriter(id) ?? ''),
            onSelect: selectable ? () => setSelected(id) : undefined,
          },
        }
      })
    },
    [laid, near, lit, graph.edges, health],
  )

  const edges = useMemo(
    () =>
      laid.edges.map(e => {
        const touches = lit != null && (e.source === lit || e.target === lit)
        return {
          ...e,
          data: { ...e.data!, showLabel: touches, dimmed: hovered != null && !touches },
        }
      }),
    [laid, hovered, lit],
  )

  // Fit on load and whenever the picture changes — or the toolbar's Fit button is pressed.
  useEffect(() => {
    const timer = window.setTimeout(() => {
      void fitView({ padding: 0.06, maxZoom: 1, duration: 200 })
    }, 0)
    return () => window.clearTimeout(timer)
  }, [fitView, graph, showHandMade, unlimited, fitKey, selected])

  // Escape closes the panel, as a dialog would.
  useEffect(() => {
    if (selected == null) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setSelected(null)
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [selected])

  const focus = graph.nodes.find(n => n.id === selected) ?? null
  const incoming = useMemo(
    () =>
      focus == null
        ? []
        : laid.edges.filter(e => e.target === focus.id).flatMap(e => e.data?.jobs ?? []),
    [laid, focus],
  )

  return (
    <>
      <div
        className={`relative w-full overflow-hidden rounded-[10px] border border-border bg-bg-soft/30${
          focus != null ? ' md:pr-[352px]' : ''
        }`}
        style={{ height }}
      >
        <Arrowheads />
        <ReactFlow
          nodes={nodes}
          edges={edges}
          nodeTypes={nodeTypes}
          edgeTypes={edgeTypes}
          className="lineage-flow"
          fitView
          fitViewOptions={{ padding: 0.06, maxZoom: 1 }}
          minZoom={0.3}
          maxZoom={1.5}
          nodesDraggable={false}
          nodesConnectable={false}
          nodesFocusable={false}
          elementsSelectable={false}
          panOnScroll
          proOptions={{ hideAttribution: true }}
          onPaneClick={() => setSelected(null)}
          onNodeClick={(_event, node) => {
            if (node.type === 'more') {
              setUnlimited(prev => new Set([...prev, Number(node.data.level)]))
              return
            }
            const data = node.data as { node: LineageNodeView }
            if (data.node.visible) setSelected(node.id)
          }}
          onNodeMouseEnter={(_event, node) => setHovered(node.id)}
          onNodeMouseLeave={() => setHovered(null)}
        >
          <Controls showInteractive={false} position="bottom-left" />
        </ReactFlow>
        {focus && (
          <NodePanel
            node={focus}
            jobs={incoming}
            days={days}
            catalog={catalog}
            onClose={() => setSelected(null)}
            onPick={onPick}
          />
        )}
      </div>
      <p className="mt-2 text-xs text-text-mute">Point at a line or select a table to see the jobs that move the data.</p>
    </>
  )
}

/** The panel a selected table opens: who makes it, who reads it, and the two ways out. */
function NodePanel({
  node,
  jobs,
  days,
  catalog,
  onClose,
  onPick,
}: {
  node: LineageNodeView
  jobs: EdgeJob[]
  days: number
  catalog: string
  onClose: () => void
  onPick: (catalog: string, schema: string, table: string) => void
}) {
  const { catalog: nsCatalog, schema } = splitNamespace(node.namespace)
  const readers = node.readers
  // The catalog page needs the catalog the page knows when the namespace does not name one.
  const tableCatalog = nsCatalog || catalog

  return (
    <aside
      aria-label={`${node.name} lineage`}
      className="absolute inset-y-3 right-3 w-[340px] max-w-[calc(100%-1.5rem)] overflow-auto rounded-[10px] border border-border bg-bg-elev p-4 shadow-xl"
    >
      <div className="flex items-start justify-between gap-2">
        <div className="min-w-0">
          <div className="truncate font-mono text-sm font-semibold">{node.name}</div>
          <div className="truncate text-xs text-text-mute">{schema}</div>
        </div>
        <button
          type="button"
          className="btn-ghost px-2 py-0.5 text-xs"
          onClick={onClose}
          aria-label="Close the table panel"
        >
          Close
        </button>
      </div>

      <div className="mt-4">
        <div className="label mb-1">Made by</div>
        {jobs.length > 0 ? (
          <div className="flex flex-wrap gap-1">
            {jobs.map(job => (
              <JobPill key={`${job.kind}|${job.name}`} job={job} />
            ))}
          </div>
        ) : (
          <p className="text-xs text-text-mute">Nothing wrote this table in {days} days.</p>
        )}
      </div>

      <div className="mt-4">
        <div className="label mb-1">Used by</div>
        {readers ? (
          <p className="text-xs text-text-dim">
            {formatCount(readers.people)} {readers.people === 1 ? 'person' : 'people'} ·{' '}
            {formatCount(readers.queries)} {readers.queries === 1 ? 'query' : 'queries'}
            {readers.tools && readers.tools.length > 0 && <> · {readers.tools.join(', ')}</>}
          </p>
        ) : (
          <p className="text-xs text-text-mute">No reads in {days} days.</p>
        )}
      </div>

      <div className="mt-4 flex flex-wrap gap-2">
        <Link
          to={`/catalog?${new URLSearchParams({ catalog: tableCatalog, ns: schema, tbl: node.name }).toString()}`}
          className="btn-ghost"
        >
          Open table
        </Link>
        <button type="button" className="btn-ghost" onClick={() => onPick(tableCatalog, schema, node.name)}>
          Center here
        </button>
      </div>
    </aside>
  )
}
