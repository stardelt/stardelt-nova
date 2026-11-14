/**
 * One timeline for the Overview (stardelt-nova#510, slice 1): the runs, the queries and the
 * jobs the API already answers are merged here, in the browser, into one list.
 *
 * Nothing is added on the server: each of the three endpoints keeps enforcing its own
 * visibility rule (a run of a pipeline the caller cannot see never reaches `/api/runs`, the
 * query log answers `scope=mine`), so the merge only orders and describes what the caller was
 * already allowed to read.
 */

import type { Job, QueryLogItem, RunsListRow } from './api'
import { formatDurationMs, rowsLabel, statusLabel } from './format'

/** One row of the Overview's merged timeline (nova#510). */
export type ActivityRow = {
  /** Unique across the three sources: `run:<ns>/<name>:<runId>`, `query:<id>`, `job:<id>`. */
  id: string
  kind: 'pipeline' | 'notebook' | 'query' | 'load'
  /** The one-line summary: what happened. */
  title: string
  /** The second line: state, duration, rows or the actor. Empty when there is none. */
  detail: string
  /** Epoch milliseconds, for sorting and for `relativeTime`. */
  at: number
  /** Where a click goes. */
  href: string
}

/** The first parseable time of a run: it started, or at least its logical date. */
function runTime(run: RunsListRow): number | null {
  for (const value of [run.startedAt, run.logicalDate]) {
    if (!value) continue
    const time = Date.parse(value)
    if (Number.isFinite(time)) return time
  }
  return null
}

/** A run's kind as the timeline names it: a single-statement pipeline ran a query. */
function runKind(kind: RunsListRow['kind']): ActivityRow['kind'] {
  if (kind === 'sql') return 'query'
  if (kind === 'notebook') return 'notebook'
  return 'pipeline'
}

/** Parts joined with ` · `, empty ones dropped, so a row never ends in a separator. */
function joinDetail(parts: (string | null | undefined)[]): string {
  return parts.filter((part): part is string => Boolean(part)).join(' · ')
}

/** A statement as one line: single-spaced and cut at 120 characters. */
function oneLine(sql: string): string {
  return sql.replace(/\s+/g, ' ').trim().slice(0, 120)
}

/** Merge the three answers into one list, newest first, at most `limit` rows. */
export function mergeActivity(
  runs: RunsListRow[] | undefined,
  queries: QueryLogItem[] | undefined,
  jobs: Job[] | undefined,
  limit = 20,
): ActivityRow[] {
  const rows: ActivityRow[] = []

  for (const run of runs ?? []) {
    const at = runTime(run)
    // A run with no readable time cannot be placed in a timeline; dropping it keeps `NaN`
    // out of the sort, where it would poison the order of every other row.
    if (at === null) continue
    rows.push({
      id: `run:${run.namespace}/${run.name}:${run.runId}`,
      kind: runKind(run.kind),
      title: `${run.name}: ${run.state}`,
      detail: joinDetail([
        run.durationSeconds != null ? formatDurationMs(run.durationSeconds * 1000) : null,
        run.triggeredBy,
      ]),
      at,
      href: `/pipelines/${run.namespace}/${run.name}`,
    })
  }

  for (const query of queries ?? []) {
    if (!Number.isFinite(query.createdAt)) continue
    rows.push({
      id: `query:${query.id}`,
      kind: 'query',
      title: oneLine(query.sql),
      detail: joinDetail([
        statusLabel(query.state),
        query.wallMs != null ? formatDurationMs(query.wallMs) : null,
      ]),
      at: query.createdAt,
      href: `/queries?id=${encodeURIComponent(query.id)}`,
    })
  }

  for (const job of jobs ?? []) {
    if (!Number.isFinite(job.started_at)) continue
    rows.push({
      id: `job:${job.id}`,
      kind: 'load',
      title: job.title,
      detail: joinDetail([
        statusLabel(job.status),
        job.rows != null ? rowsLabel(job.rows) : null,
      ]),
      at: job.started_at,
      href: '/data',
    })
  }

  // Newest first; the id breaks ties so two answers with the same time keep one order
  // between renders instead of swapping places.
  rows.sort((a, b) => (b.at - a.at) || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))
  return rows.slice(0, limit)
}
