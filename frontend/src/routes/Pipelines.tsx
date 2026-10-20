import { useRef, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { ApiError, api, errorMessage, formatTime, type PipelineView } from '../api'
import ErrorBox from '../components/ErrorBox'
import { Icon } from '../components/Icon'
import { formatDurationMs, relativeTime } from '../format'

/**
 * Pipelines (J1): the scheduled `Pipeline` resources of the caller's groups, what each
 * one last did, and the two actions that make sense from a list — run now, pause or
 * resume. The detail page (`PipelineDetail.tsx`) owns runs, tasks, logs and stopping a
 * run.
 *
 * The API is the policy point: it already refuses what the scheduler would silently
 * misbehave on (a paused pipeline, an active run under `Forbid`). This page shows the
 * API's sentence verbatim; it never invents a rule of its own.
 */

type PipelineAction = 'run' | 'pause' | 'resume'

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

/**
 * Run now / pause / resume, shared by the list and the detail so the two cannot drift.
 * One request per press: a second press while one is in flight is ignored.
 */
export function usePipelineActions(refetch: () => Promise<unknown> | void) {
  const [notice, setNotice] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [pending, setPending] = useState<string | null>(null)
  const inFlight = useRef(false)

  async function act(pipeline: PipelineView, action: PipelineAction) {
    if (inFlight.current) return
    inFlight.current = true
    setPending(`${action}:${pipeline.name}`)
    setError(null)
    setNotice(null)
    try {
      if (action === 'run') {
        await api.startPipelineRun(pipeline.namespace, pipeline.name)
        setNotice(`Started a run of ${pipeline.name}.`)
      } else if (action === 'pause') {
        await api.pausePipeline(pipeline.namespace, pipeline.name)
      } else {
        await api.resumePipeline(pipeline.namespace, pipeline.name)
      }
      await refetch()
    } catch (e: unknown) {
      // The API's own sentence — the paused and already-active refusals are written for
      // this page and are not paraphrased here.
      setError(errorMessage(e))
    } finally {
      inFlight.current = false
      setPending(null)
    }
  }

  return {
    notice,
    error,
    pending,
    act,
    /** Say something a control outside `act` did (the stop dialog). */
    announce: setNotice,
    clearError: () => setError(null),
  }
}

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

export function PipelineActions({
  pipeline,
  pending,
  onAct,
}: {
  pipeline: PipelineView
  pending: string | null
  onAct: (pipeline: PipelineView, action: PipelineAction) => void | Promise<void>
}) {
  const disabled = pending !== null
  return (
    <div className="flex flex-wrap items-center gap-2">
      <button
        type="button"
        className="btn-ghost py-1.5 text-xs"
        aria-label={`Run now ${pipeline.name}`}
        disabled={disabled || pipeline.paused}
        title={pipeline.paused ? 'Resume the pipeline first' : undefined}
        onClick={() => void onAct(pipeline, 'run')}
      >
        <Icon name="play" className="w-3.5 h-3.5" />
        Run now
      </button>
      {pipeline.paused ? (
        <button
          type="button"
          className="btn-ghost py-1.5 text-xs"
          aria-label={`Resume ${pipeline.name}`}
          disabled={disabled}
          onClick={() => void onAct(pipeline, 'resume')}
        >
          Resume
        </button>
      ) : (
        <button
          type="button"
          className="btn-ghost py-1.5 text-xs"
          aria-label={`Pause ${pipeline.name}`}
          disabled={disabled}
          onClick={() => void onAct(pipeline, 'pause')}
        >
          Pause
        </button>
      )}
    </div>
  )
}

export default function Pipelines() {
  const queryClient = useQueryClient()
  const list = useQuery({
    queryKey: ['pipelines'],
    queryFn: api.pipelines,
    // Quick while something is moving; a quiet list does not need a request every 10 s.
    refetchInterval: (query) => {
      const rows = query.state.data?.pipelines ?? []
      return rows.some((p) => p.state === 'running' || p.state === 'loading') ? 10_000 : 30_000
    },
  })
  const actions = usePipelineActions(() => queryClient.invalidateQueries({ queryKey: ['pipelines'] }))

  const pipelines = list.data?.pipelines ?? []
  const failed = list.isError && !list.data
  // Without a cluster or a scheduler every pipeline route answers 503: the sentence is a
  // fact about the installation, not a failure a Retry could fix.
  const schedulerMissing = failed && list.error instanceof ApiError && list.error.status === 503

  return (
    <div className="max-w-6xl space-y-5">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Pipelines</h1>
        <p className="text-text-dim text-sm mt-1">Scheduled work you and your groups own.</p>
      </header>

      <PipelineFeedback notice={actions.notice} error={actions.error} />

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
          <p className="px-5 py-5 text-sm text-text-mute">
            No pipelines yet. Pipelines you or your groups own appear here.
          </p>
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
                <th scope="col" className="th w-[11rem] px-3 py-2.5">
                  <span className="sr-only">Actions</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {pipelines.map((pipeline) => (
                <Row
                  key={`${pipeline.namespace}/${pipeline.name}`}
                  pipeline={pipeline}
                  pending={actions.pending}
                  onAct={actions.act}
                />
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  )
}

function Row({
  pipeline,
  pending,
  onAct,
}: {
  pipeline: PipelineView
  pending: string | null
  onAct: (pipeline: PipelineView, action: PipelineAction) => void | Promise<void>
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
      <td className="td px-3">
        <div className="flex justify-end">
          <PipelineActions pipeline={pipeline} pending={pending} onAct={onAct} />
        </div>
      </td>
    </tr>
  )
}
