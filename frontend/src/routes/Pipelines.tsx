import { useEffect, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link, useLocation, useNavigate } from 'react-router-dom'
import { ApiError, api, type PipelineView } from '../api'
import { EmptyState } from '../components/EmptyState'
import ErrorBox from '../components/ErrorBox'
import { Icon } from '../components/Icon'
import ScheduleDialog from '../components/ScheduleDialog'
import { formatDurationMs, formatTime, relativeTime } from '../format'

/**
 * Pipelines (J1): the scheduled `Pipeline` resources of the caller's groups, what each
 * one last did. Actions live in Airflow — use the Open in Airflow link on the detail
 * page.
 *
 * The API is the policy point: it already refuses what the scheduler would silently
 * misbehave on (a paused pipeline, an active run under `Forbid`). This page shows the
 * API's sentence verbatim; it never invents a rule of its own.
 */



type StateLook = { word: string; icon: string; cls: string }

const NEUTRAL = 'border-border bg-bg-soft text-text-dim'
const GOOD = 'border-emerald-400/40 bg-emerald-400/10 text-ok'
const BAD = 'border-red-400/40 bg-red-400/10 text-bad'
const BUSY = 'border-sky-400/40 bg-sky-400/10 text-info'
const WAIT = 'border-amber-400/40 bg-amber-400/10 text-warn'

/** The state words of `PipelineView.state`; the icon carries no meaning without the word. */
const PIPELINE_STATES: Record<string, StateLook> = {
  loading: { word: 'Loading into the scheduler', icon: '◌', cls: BUSY },
  'not-loaded': { word: 'Not loaded', icon: '!', cls: WAIT },
  paused: { word: 'Paused', icon: '‖', cls: WAIT },
  running: { word: 'Running', icon: '◔', cls: BUSY },
  failed: { word: 'Failed', icon: '✕', cls: BAD },
  succeeded: { word: 'Succeeded', icon: '✓', cls: GOOD },
  idle: { word: 'Not run yet', icon: '·', cls: NEUTRAL },
}

/** The state words of a run or task instance. */
const RUN_STATES: Record<string, { word: string; cls: string }> = {
  queued: { word: 'Queued', cls: BUSY },
  running: { word: 'Running', cls: BUSY },
  success: { word: 'Succeeded', cls: GOOD },
  failed: { word: 'Failed', cls: BAD },
  upstream_failed: { word: 'Upstream failed', cls: BAD },
  skipped: { word: 'Skipped', cls: WAIT },
  up_for_retry: { word: 'Up for retry', cls: WAIT },
}

/** `not-loaded` also tells a person what to do about it. */
export const NOT_LOADED_NOTE =
  'The scheduler did not accept this pipeline. Ask an administrator to check the loader log.'

export function PipelineStateBadge({ state }: { state: string }) {
  const look = PIPELINE_STATES[state] ?? { word: state || 'Unknown', icon: '•', cls: NEUTRAL }
  return (
    <span className={`badge ${look.cls}`}>
      <span aria-hidden>{look.icon}</span>
      {look.word}
    </span>
  )
}

export function runStateWord(state: string): string {
  return RUN_STATES[state]?.word ?? (state || 'Unknown')
}

export function RunStateBadge({ state }: { state: string }) {
  return <span className={`badge ${RUN_STATES[state]?.cls ?? NEUTRAL}`}>{runStateWord(state)}</span>
}

/**
 * A relative time with the exact time in `title`. An absent or malformed timestamp is a
 * dash: the API sends RFC 3339, but nothing here may print `Invalid Date` if it does not.
 */
export function formatWhen(value: string | null | undefined): { text: string; title?: string } {
  if (!value) return { text: '—' }
  const ms = Date.parse(value)
  return Number.isFinite(ms) ? { text: relativeTime(ms), title: formatTime(ms) } : { text: '—' }
}

/** The run's duration like the Query log's: `850 ms`, `2.3 s`, `1 m 04 s`. */
export function runDuration(seconds: number | null | undefined): string {
  return formatDurationMs(seconds == null ? null : seconds * 1000)
}

/** Pipeline feedback notices and errors shown on the list/detail pages. */

export function PipelineFeedback({ notice, error }: { notice: string | null; error: string | null }) {
  return (
    <>
      {notice && <p role="status" className="text-sm text-ok break-words">{notice}</p>}
      {error && (
        <div role="alert" className="rounded-[10px] border border-red-500/40 bg-red-500/10 p-3 text-sm text-bad break-words">
          {error}
        </div>
      )}
    </>
  )
}



export default function Pipelines() {
  const location = useLocation()
  const navigate = useNavigate()
  const [routedNotice, setRoutedNotice] = useState<string | null>(null)
  const [scheduling, setScheduling] = useState(false)
  const list = useQuery({
    queryKey: ['pipelines'],
    queryFn: api.pipelines,
    // Quick while something is moving; a quiet list does not need a request every 10 s.
    refetchInterval: (query) => {
      const rows = query.state.data?.pipelines ?? []
      return rows.some((p) => p.state === 'running' || p.state === 'loading') ? 10_000 : 30_000
    },
  })
  const principals = useQuery({
    queryKey: ['service-principals'],
    queryFn: api.servicePrincipals,
    refetchInterval: 60_000,
  })
  // A delete on the detail page comes back here with its notice in the route state; show it
  // once, then replace the entry so Back does not bring the sentence along.
  useEffect(() => {
    const state = location.state as { notice?: unknown } | null
    if (typeof state?.notice !== 'string') return
    setRoutedNotice(state.notice)
    navigate(location.pathname, { replace: true, state: null })
  }, [location.pathname, location.state, navigate])

  const pipelines = list.data?.pipelines ?? []
  const failed = list.isError && !list.data
  // Without a cluster or a scheduler every pipeline route answers 503: the sentence is a
  // fact about the installation, not a failure a Retry could fix.
  const schedulerMissing = failed && list.error instanceof ApiError && list.error.status === 503

  return (
    <div className="max-w-6xl space-y-5">
      <header className="flex flex-wrap items-start justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-2xl font-bold tracking-tight">Pipelines</h1>
          <p className="text-text-dim text-sm mt-1">Scheduled work you and your groups own.</p>
        </div>
        <button type="button" className="btn-primary" onClick={() => setScheduling(true)}>
          <Icon name="clock" className="w-4 h-4" />
          Schedule a notebook
        </button>
      </header>

      <PipelineFeedback notice={routedNotice} error={null} />

      <div className="card relative overflow-x-auto">
        {list.isLoading && <p className="px-5 py-5 text-sm text-text-mute">Loading pipelines…</p>}
        {failed && (
          <div className="px-5 py-5 space-y-2">
            <ErrorBox
              error={list.error}
              onRetry={schedulerMissing ? undefined : () => void list.refetch()}
            />
            {schedulerMissing && (
              <p className="text-sm text-text-mute">
                An administrator has to set up the scheduler connection for this installation.
              </p>
            )}
          </div>
        )}
        {list.data && pipelines.length === 0 && (
          <EmptyState icon="lineage" title="No pipelines yet">
            <Link to="/query" className="underline">Open the SQL editor</Link>, write a statement and press Schedule.
          </EmptyState>
        )}
        {list.data && pipelines.length > 0 && (
          <table className="w-full text-sm">
            <caption className="sr-only">Pipelines you and your groups own.</caption>
            <thead className="bg-bg-soft/50">
              <tr>
                <th scope="col" className="th w-[14rem] px-5 py-2.5">Name</th>
                <th scope="col" className="th px-3 py-2.5">Owner</th>
                <th scope="col" className="th px-3 py-2.5">Schedule</th>
                <th scope="col" className="th px-3 py-2.5">State</th>
                <th scope="col" className="th px-3 py-2.5">Last run</th>
                <th scope="col" className="th px-3 py-2.5">Next run</th>
              </tr>
            </thead>
            <tbody>
              {pipelines.map((pipeline) => (
                <Row
                  key={`${pipeline.namespace}/${pipeline.name}`}
                  pipeline={pipeline}
                />
              ))}
            </tbody>
          </table>
        )}
      </div>

      <div className="card relative overflow-x-auto">
        <div className="px-5 pt-5">
          <h2 className="text-lg font-semibold">Service principals</h2>
          <p className="text-text-dim text-sm">
            How your groups' jobs and maintenance tasks sign in without a person.
          </p>
        </div>
        {principals.isLoading && (
          <p className="px-5 py-5 text-sm text-text-mute">Loading service principals…</p>
        )}
        {principals.isError && (
          <div className="px-5 py-5">
            <ErrorBox error={principals.error} onRetry={() => void principals.refetch()} />
          </div>
        )}
        {principals.data && (
          <>
            {(() => {
              const rows = principals.data?.principals ?? []
              if (rows.length === 0) {
                return (
                  <p className="px-5 py-5 text-sm text-text-mute">
                    No service principals in your groups yet. An administrator creates one when a group needs a non-human identity.
                  </p>
                )
              }
              return (
                <table className="w-full text-sm">
                  <caption className="sr-only">Service principals of your groups.</caption>
                  <thead className="bg-bg-soft/50">
                    <tr>
                      <th scope="col" className="th w-[14rem] px-5 py-2.5">
                        Name
                      </th>
                      <th scope="col" className="th px-3 py-2.5">
                        Group
                      </th>
                      <th scope="col" className="th px-3 py-2.5">
                        Purpose
                      </th>
                      <th scope="col" className="th px-3 py-2.5">
                        Audiences
                      </th>
                      <th scope="col" className="th px-3 py-2.5">
                        State
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map((sp) => (
                      <tr key={sp.name} className="border-t border-border/70 hover:bg-bg-soft/30">
                        <td className="td px-5">
                          <span className="font-mono text-xs">{sp.name}</span>
                        </td>
                        <td className="td px-3 text-text-dim">
                          <span className="whitespace-nowrap">{sp.ownerGroup}</span>
                        </td>
                        <td className="td px-3">{sp.purpose}</td>
                        <td className="td px-3">
                          {sp.audiences && sp.audiences.length > 0 ? sp.audiences.join(', ') : '—'}
                        </td>
                        <td className="td px-3">
                          {sp.ready ? (
                            <span className={`badge ${GOOD}`}>Ready</span>
                          ) : (
                            <>
                              <span className={`badge ${WAIT}`}>Not ready</span>
                              <p className="mt-1 max-w-[22rem] text-xs text-text-mute break-words">
                                {sp.message ?? 'no status yet'}
                              </p>
                            </>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )
            })()}
          </>
        )}
      </div>

      {scheduling && (
        <ScheduleDialog
          mode="create"
          initialKind="notebook"
          onClose={() => setScheduling(false)}
          onScheduled={(view) => {
            setScheduling(false)
            navigate('/pipelines', { state: { notice: `Scheduled ${view.name}.` } })
          }}
        />
      )}
    </div>
  )
}

function Row({
  pipeline,
}: {
  pipeline: PipelineView
}) {
  const last = pipeline.lastRun
  const lastWhen = last ? formatWhen(last.startedAt ?? last.logicalDate ?? last.endedAt) : null
  const next = formatWhen(pipeline.nextRun)
  return (
    <tr className="border-t border-border/70 hover:bg-bg-soft/30">
      <td className="td px-5">
        <Link
          to={`/pipelines/${encodeURIComponent(pipeline.namespace)}/${encodeURIComponent(pipeline.name)}`}
          className="underline break-words"
        >
          {pipeline.name}
        </Link>
      </td>
      <td className="td px-3 text-text-dim">
        <span className="whitespace-nowrap">{pipeline.ownerGroup ?? '—'}</span>
      </td>
      <td className="td px-3">
        <span className="font-mono text-xs break-words">{pipeline.schedule ?? 'On demand'}</span>
      </td>
      <td className="td px-3">
        <PipelineStateBadge state={pipeline.state} />
        {pipeline.state === 'not-loaded' && (
          <p className="mt-1 max-w-[18rem] text-xs text-text-mute break-words">{NOT_LOADED_NOTE}</p>
        )}
      </td>
      <td className="td px-3">
        {last ? (
          <div className="min-w-0">
            <RunStateBadge state={last.state} />
            <div className="mt-0.5 text-xs text-text-mute whitespace-nowrap">
              <span className="whitespace-nowrap" title={lastWhen?.title}>{lastWhen?.text}</span>
              {' · '}
              <span className="whitespace-nowrap">{runDuration(last.durationSeconds)}</span>
            </div>
          </div>
        ) : (
          <span className="text-text-mute">—</span>
        )}
      </td>
      <td className="td px-3 text-text-dim text-xs whitespace-nowrap">
        <span title={next.title}>{next.text}</span>
      </td>
    </tr>
  )
}
