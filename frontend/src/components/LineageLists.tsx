import { useState } from 'react'
import { Link } from 'react-router-dom'
import type { LineageEdgeView, LineageGraphView, LineageJobView, LineageNodeView } from '../api'
import { formatTime } from '../format'
import { Icon } from './Icon'

/**
 * The two lists of a table's lineage (stardelt-nova#703): "Made from" on the left, "Used by" on
 * the right. One line per table — the table, the job that moves data into or out of it, and the
 * steps from the focus — so a person can scan it. Tables a query made by hand are folded away
 * behind one button; tables the person may not read collapse into one muted line.
 *
 * The same component is meant for the graph page on a phone, so it takes the whole graph and
 * does not read the tab's state.
 */

/** A lineage node's table page: the namespace is `<catalog>.<schema>` (or just a schema in demo data). */
export function lineageHref(n: { namespace: string; name: string }): string {
  const dot = n.namespace.indexOf('.')
  const catalog = dot > 0 ? n.namespace.slice(0, dot) : ''
  const schema = dot > 0 ? n.namespace.slice(dot + 1) : n.namespace
  const q = new URLSearchParams({ schema, table: n.name })
  if (catalog) q.set('catalog', catalog)
  return `/lineage?${q.toString()}`
}

/** The job behind a lineage edge: a pipeline is a link, a person is the name they read. */
export function JobPill({ job, label }: { job: { kind: string; name: string }; label?: string }) {
  const icon = job.kind === 'pipeline' ? 'play' : job.kind === 'notebook' ? 'notebook' : 'list'
  const body = (
    <>
      <Icon name={icon} className="w-3.5 h-3.5 shrink-0" />
      <span className="font-mono truncate max-w-[16rem]">{label ?? job.name}</span>
    </>
  )
  // A pipeline job is named `<namespace>/<name>`, which is its page.
  if (job.kind === 'pipeline') {
    return (
      <Link
        to={`/pipelines/${job.name.split('/').map(encodeURIComponent).join('/')}`}
        className="inline-flex items-center gap-1 rounded-full border border-border px-2 py-0.5 text-xs text-accent hover:underline max-w-full"
        title={`Pipeline ${job.name}`}
      >
        {body}
      </Link>
    )
  }
  const what = job.kind === 'notebook' ? 'Notebook' : job.kind === 'sql-editor' ? 'SQL editor' : job.kind === 'query' ? 'Query' : job.kind
  return (
    <span
      className="inline-flex items-center gap-1 rounded-full border border-border px-2 py-0.5 text-xs text-text-dim max-w-full"
      title={`${what} · ${job.name}`}
    >
      {body}
    </span>
  )
}

/** The schema part of a lineage namespace: `<catalog>.<schema>` → `schema`. */
function schemaOf(namespace: string): string {
  const dot = namespace.indexOf('.')
  return dot > 0 ? namespace.slice(dot + 1) : namespace
}

/**
 * A row's pill names the job that connects the table. A pipeline in the table's own namespace
 * loses its prefix (`marts/daily-revenue` reads `daily-revenue` beside `marts.daily_revenue`);
 * every other pipeline keeps it, because the namespace is then the only thing telling jobs apart.
 */
function pillLabel(job: { kind: string; name: string }, namespace: string): string | undefined {
  if (job.kind !== 'pipeline') return undefined
  const slash = job.name.indexOf('/')
  if (slash <= 0) return undefined
  return job.name.slice(0, slash) === schemaOf(namespace) ? job.name.slice(slash + 1) : undefined
}

/** The table's line: `schema.table`, with the catalog only when it is not the focus catalog. */
function tableLabel(n: LineageNodeView, catalog: string): string {
  const dot = n.namespace.indexOf('.')
  const nsCatalog = dot > 0 ? n.namespace.slice(0, dot) : ''
  const schema = dot > 0 ? n.namespace.slice(dot + 1) : n.namespace
  const name = `${schema}.${n.name}`
  return nsCatalog && nsCatalog !== catalog ? `${nsCatalog}.${name}` : name
}

type Direction = 'up' | 'down'

export function LineageLists({ graph, days, catalog }: { graph: LineageGraphView; days: number; catalog: string }) {
  const upstream = graph.nodes.filter(n => n.visible && (n.level ?? 0) < 0)
  const downstream = graph.nodes.filter(n => n.visible && (n.level ?? 0) > 0)
  // A node the person may not read is no row: its edge direction says which list it belongs to.
  const hiddenOn = (direction: Direction) =>
    graph.nodes.filter(n => {
      if (n.visible) return false
      const edges = graph.edges.filter(e => (direction === 'up' ? e.from === n.id : e.to === n.id))
      return edges.length > 0
    }).length

  return (
    <div className="grid md:grid-cols-2 gap-4">
      <List
        title="Made from"
        empty={`Nothing feeds this table in ${days} days.`}
        nodes={upstream}
        hidden={hiddenOn('up')}
        direction="up"
        graph={graph}
        catalog={catalog}
      />
      <List
        title="Used by"
        empty={`No table is built from this one in ${days} days.`}
        nodes={downstream}
        hidden={hiddenOn('down')}
        direction="down"
        graph={graph}
        catalog={catalog}
        note={
          graph.impact.downstream > downstream.length
            ? {
                testId: 'lineage-impact-downstream',
                text: `${graph.impact.downstream} tables at every step · ${downstream.length} drawn`,
              }
            : undefined
        }
      />
    </div>
  )
}

function List({
  title,
  empty,
  nodes,
  hidden,
  direction,
  graph,
  catalog,
  note,
}: {
  title: string
  empty: string
  nodes: LineageNodeView[]
  hidden: number
  direction: Direction
  graph: LineageGraphView
  catalog: string
  /** A line under the heading, when the drawing shows fewer tables than exist. */
  note?: { testId: string; text: string }
}) {
  const [showHandMade, setShowHandMade] = useState(false)
  const level = (n: LineageNodeView) => Math.abs(n.level ?? 0)
  const sorted = [...nodes].sort((a, b) => level(a) - level(b) || a.name.localeCompare(b.name))
  // A table a query made by hand is no consumer or source of the plan: it sits at the end, folded.
  const byHand = sorted.filter(n => n.handMade)
  const groups: Array<{ step: number; rows: LineageNodeView[] }> = []
  for (const n of sorted.filter(n => !n.handMade)) {
    const step = level(n)
    const last = groups[groups.length - 1]
    if (last && last.step === step) last.rows.push(n)
    else groups.push({ step, rows: [n] })
  }

  return (
    <div className="card p-5 min-w-0">
      <div className="label mb-2">{title}</div>
      {note && (
        <p className="text-xs text-text-mute mb-2" data-testid={note.testId}>
          {note.text}
        </p>
      )}
      {sorted.length === 0 && hidden === 0 ? (
        <p className="text-sm text-text-mute">{empty}</p>
      ) : (
        <div className="space-y-3">
          {groups.map((g) => (
            <div key={g.step}>
              {g.step > 1 && <div className="text-[11px] uppercase tracking-widest text-text-mute mb-1">{g.step} steps away</div>}
              <ul className="space-y-1">
                {g.rows.map(n => (
                  <Row key={n.id} node={n} direction={direction} graph={graph} catalog={catalog} muted={false} />
                ))}
              </ul>
            </div>
          ))}
          {showHandMade && byHand.length > 0 && (
            <ul className="space-y-1">
              {byHand.map(n => (
                <Row key={n.id} node={n} direction={direction} graph={graph} catalog={catalog} muted />
              ))}
            </ul>
          )}
          {byHand.length > 0 && (
            <button
              type="button"
              className="btn-ghost py-1 px-2 text-xs"
              onClick={() => setShowHandMade(v => !v)}
            >
              {showHandMade ? 'Hide tables made by hand' : `＋ ${byHand.length} ${byHand.length === 1 ? 'table' : 'tables'} made by hand`}
            </button>
          )}
          {hidden > 0 && (
            <p className="text-xs text-text-mute">{hidden} {hidden === 1 ? 'table' : 'tables'} you cannot see</p>
          )}
        </div>
      )}
    </div>
  )
}

function Row({
  node,
  direction,
  graph,
  catalog,
  muted,
}: {
  node: LineageNodeView
  direction: Direction
  graph: LineageGraphView
  catalog: string
  muted: boolean
}) {
  const edges = graph.edges.filter(e => (direction === 'up' ? e.from === node.id : e.to === node.id))
  const edge = newestOf(edges)
  const job = edge ? jobOf(edge, graph.jobs) : undefined
  return (
    <li
      className={['flex flex-wrap items-center gap-x-2 gap-y-1 min-w-0 py-0.5', muted ? 'opacity-60' : ''].join(' ')}
      title={formatTime(node.lastWrite ?? '')}
    >
      <Link
        to={lineageHref(node)}
        className={[
          'text-sm hover:underline font-mono break-all min-w-0',
          muted ? 'text-text-mute' : 'text-accent',
        ].join(' ')}
      >
        {tableLabel(node, catalog)}
      </Link>
      {job && <JobPill job={job} label={pillLabel(job, node.namespace)} />}
    </li>
  )
}

function newestOf(edges: LineageEdgeView[]): LineageEdgeView | undefined {
  return [...edges].sort((a, b) => b.eventTime.localeCompare(a.eventTime))[0]
}

/** Each edge names its job by namespace and name; a graph that lists no jobs still gets a pill. */
function jobOf(edge: LineageEdgeView, jobs: LineageJobView[]): { kind: string; name: string } {
  const job = jobs.find(j => j.id === `${edge.jobNamespace}|${edge.jobName}`)
  return { kind: job?.kind ?? edge.jobNamespace, name: job?.name ?? edge.jobName }
}
