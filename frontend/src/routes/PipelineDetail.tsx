import { Fragment, useState } from 'react'
import { useInfiniteQuery, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { ApiError, api, type LogLine, type PipelineDetail as PipelineDetailView, type RunView } from '../api'
import ErrorBox from '../components/ErrorBox'
import ScheduleDialog, { DeletePipelineDialog } from '../components/ScheduleDialog'
import StopRunDialog from '../components/StopRunDialog'
import { shortVersion } from '../format'
import {
  NOT_LOADED_NOTE,
  PipelineActions,
  PipelineFeedback,
  PipelineStateBadge,
  RunStateBadge,
  formatWhen,
  runDuration,
  usePipelineActions,
} from './Pipelines'

/**
 * One pipeline: its owner's facts, the declared tasks, the latest runs and — under a
 * run — its task instances and their logs. Run now, pause, resume and stop live here
 * too, sharing their implementation with the list.
 *
 * A run that is still `queued` or `running` is read again every 5 s. A log is read in
 * pages and rendered as plain text: no markup, no links made from log text.
 */
export default function PipelineDetail() {
  const { namespace = '', name = '' } = useParams()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [openRun, setOpenRun] = useState<string | null>(null)
  const [stopping, setStopping] = useState<RunView | null>(null)
  const [editing, setEditing] = useState(false)
  const [deleting, setDeleting] = useState(false)

  const detail = useQuery({
    queryKey: ['pipeline', namespace, name],
    queryFn: () => api.pipeline(namespace, name),
    refetchInterval: (query) => {
      const runs = query.state.data?.runs ?? []
      return runs.some((run) => run.state === 'queued' || run.state === 'running') ? 5_000 : false
    },
  })

  const actions = usePipelineActions(() =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: ['pipeline', namespace, name] }),
      queryClient.invalidateQueries({ queryKey: ['pipelines'] }),
    ]),
  )

  function stopped(runId: string) {
    setStopping(null)
    actions.announce(`Stopped run ${runId}.`)
    actions.clearError()
    void queryClient.invalidateQueries({ queryKey: ['pipeline', namespace, name] })
    void queryClient.invalidateQueries({ queryKey: ['pipeline-run', namespace, name, runId] })
    void queryClient.invalidateQueries({ queryKey: ['pipelines'] })
  }

  if (detail.isLoading) {
    return (
      <div className="max-w-6xl space-y-5">
        <h1 className="text-2xl font-bold tracking-tight break-words">{name}</h1>
        <p className="text-sm text-text-mute">Loading pipeline…</p>
      </div>
    )
  }

  if (detail.isError && !detail.data) {
    const status = detail.error instanceof ApiError ? detail.error.status : 0
    const notFound = status === 404
    // A 503 is a fact about the installation: no Retry, but the sentence says what to do.
    const schedulerMissing = status === 503
    return (
      <div className="max-w-2xl space-y-4">
        <h1 className="text-2xl font-bold tracking-tight">{notFound ? 'Not found' : name}</h1>
        <div className="card space-y-3 p-5">
          <ErrorBox
            error={detail.error}
            onRetry={notFound || schedulerMissing ? undefined : () => void detail.refetch()}
          />
          {schedulerMissing && (
            <p className="text-sm text-text-mute">
              An administrator has to set up the scheduler connection for this installation.
            </p>
          )}
          <div>
            <Link to="/pipelines" className="btn-ghost w-fit">Back to Pipelines</Link>
          </div>
        </div>
      </div>
    )
  }

  const pipeline = detail.data
  if (!pipeline) {
    // `isLoading` and `isError` cover every state react-query has; this only narrows the type.
    return null
  }

  return (
    <div className="max-w-6xl space-y-5">
      <header className="flex flex-wrap items-start justify-between gap-4">
        <div className="min-w-0">
          <div className="mb-1 text-xs">
            <Link to="/pipelines" className="underline text-text-dim">All pipelines</Link>
          </div>
          <h1 className="text-2xl font-bold tracking-tight break-words">{pipeline.name}</h1>
          <p className="text-text-dim text-sm mt-1">The runs, tasks and logs of this pipeline.</p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <PipelineActions pipeline={pipeline} pending={actions.pending} onAct={actions.act} />
          <button type="button" className="btn-ghost py-1.5 text-xs" onClick={() => setEditing(true)}>
            Edit schedule
          </button>
          <button
            type="button"
            className="btn-ghost py-1.5 text-xs text-bad"
            onClick={() => setDeleting(true)}
          >
            Delete pipeline
          </button>
        </div>
      </header>

      <PipelineFeedback notice={actions.notice} error={actions.error} />

      <dl className="grid grid-cols-1 gap-x-6 gap-y-3 text-sm sm:grid-cols-2 lg:grid-cols-5">
        <Fact label="State">
          <PipelineStateBadge state={pipeline.state} />
          {pipeline.state === 'not-loaded' && (
            <p className="mt-1 text-xs text-text-mute break-words">{NOT_LOADED_NOTE}</p>
          )}
        </Fact>
        <Fact label="Owner">
          <span className="break-words">{pipeline.ownerGroup ?? '—'}</span>
        </Fact>
        <Fact label="Schedule">
          <span className="font-mono text-xs break-words">{pipeline.schedule ?? 'On demand'}</span>
        </Fact>
        <Fact label="Runs as">
          <span className="font-mono text-xs break-words">{pipeline.runAs ?? '—'}</span>
        </Fact>
        <Fact label="Concurrency">
          <span className="break-words">{pipeline.concurrency || '—'}</span>
        </Fact>
      </dl>

      <section className="card p-5">
        <h2 className="text-lg font-semibold">Tasks</h2>
        {pipeline.tasks.length === 0 ? (
          <p className="mt-2 text-sm text-text-mute">This pipeline declares no tasks.</p>
        ) : (
          <ul className="mt-3 space-y-2 text-sm">
            {pipeline.tasks.map((task, index) => {
              const stored = Object.entries(task.notebookParams ?? {}).sort(([a], [b]) => a.localeCompare(b))
              return (
                <li key={`${task.name}-${index}`} className="min-w-0">
                  <div className="flex flex-wrap items-baseline gap-2">
                    <span className="font-mono text-xs break-all">{task.name}</span>
                    <span className="badge border-border bg-bg-soft text-text-dim">{task.kind || 'Unknown'}</span>
                  </div>
                  {task.kind === 'notebook' && (task.notebook || task.objectVersion) && (
                    <p className="mt-0.5 text-text-dim break-words">
                      Notebook <span className="font-mono text-xs break-all">{task.notebook || '—'}</span>
                      {' · '}version <span className="font-mono text-xs">{shortVersion(task.objectVersion)}</span>
                    </p>
                  )}
                  {stored.length > 0 && (
                    <dl className="mt-1 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-0.5 text-xs">
                      {stored.map(([paramName, value]) => (
                        <Fragment key={paramName}>
                          <dt className="font-mono text-text-mute break-all">{paramName}</dt>
                          <dd className="font-mono text-text-dim break-all">{value}</dd>
                        </Fragment>
                      ))}
                    </dl>
                  )}
                </li>
              )
            })}
          </ul>
        )}
      </section>

      <div className="[container-type:inline-size]">
      <section className="card relative overflow-x-auto">
        <div className="px-5 pt-5 pb-3">
          <h2 className="text-lg font-semibold">Runs</h2>
          <p className="text-text-mute text-sm mt-1">The latest 20 runs, newest first.</p>
        </div>
        <table className="w-full text-sm">
          <caption className="sr-only">The latest runs of {pipeline.name}.</caption>
          <thead className="bg-bg-soft/50">
            <tr>
              <th scope="col" className="th min-w-[16rem] px-5 py-2.5">Run</th>
              <th scope="col" className="th px-3 py-2.5">State</th>
              <th scope="col" className="th px-3 py-2.5">Type</th>
              <th scope="col" className="th px-3 py-2.5">Started</th>
              <th scope="col" className="th px-3 py-2.5">Duration</th>
              <th scope="col" className="th px-3 py-2.5">Triggered by</th>
              <th scope="col" className="th px-3 py-2.5">Note</th>
              <th scope="col" className="th px-3 py-2.5">
                <span className="sr-only">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {pipeline.runs.length === 0 && (
              <tr>
                <td colSpan={8} className="px-5 py-4 text-text-mute">No runs yet.</td>
              </tr>
            )}
            {pipeline.runs.map((run, index) => {
              const open = openRun === run.runId
              const panelId = `run-panel-${index}`
              const started = formatWhen(run.startedAt ?? run.logicalDate)
              const active = run.state === 'queued' || run.state === 'running'
              return (
                <RunRow
                  key={run.runId}
                  pipeline={pipeline}
                  run={run}
                  open={open}
                  panelId={panelId}
                  started={started}
                  active={active}
                  onToggle={() => setOpenRun(open ? null : run.runId)}
                  onStop={() => setStopping(run)}
                />
              )
            })}
          </tbody>
        </table>
      </section>
      </div>

      {stopping && (
        <StopRunDialog
          pipeline={pipeline}
          run={stopping}
          onClose={() => setStopping(null)}
          onStopped={stopped}
        />
      )}

      {editing && (
        <ScheduleDialog
          mode="edit"
          pipeline={pipeline}
          onClose={() => setEditing(false)}
          onScheduled={(view) => {
            setEditing(false)
            actions.clearError()
            actions.announce(`Saved ${view.name}.`)
            void queryClient.invalidateQueries({ queryKey: ['pipeline', namespace, name] })
            void queryClient.invalidateQueries({ queryKey: ['pipelines'] })
          }}
        />
      )}

      {deleting && (
        <DeletePipelineDialog
          pipeline={pipeline}
          onClose={() => setDeleting(false)}
          onDeleted={() => {
            setDeleting(false)
            void queryClient.invalidateQueries({ queryKey: ['pipelines'] })
            navigate('/pipelines', { state: { notice: `Deleted ${pipeline.name}.` } })
          }}
        />
      )}
    </div>
  )
}

function Fact({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="min-w-0">
      <dt className="label">{label}</dt>
      <dd className="mt-0.5">{children}</dd>
    </div>
  )
}

function RunRow({
  pipeline,
  run,
  open,
  panelId,
  started,
  active,
  onToggle,
  onStop,
}: {
  pipeline: PipelineDetailView
  run: RunView
  open: boolean
  panelId: string
  started: { text: string; title?: string }
  active: boolean
  onToggle: () => void
  onStop: () => void
}) {
  return (
    <>
      <tr className="border-t border-border/70 hover:bg-bg-soft/30">
        <td className="td px-5">
          <button
            type="button"
            aria-expanded={open}
            aria-controls={panelId}
            aria-label={`${open ? 'Hide' : 'Show'} tasks of run ${run.runId}`}
            onClick={onToggle}
            className="flex items-start gap-1.5 text-left"
          >
            <span aria-hidden className={`inline-block ${open ? 'rotate-90' : ''}`}>›</span>
            <span className="font-mono text-xs break-all">{run.runId}</span>
          </button>
        </td>
        <td className="td px-3"><RunStateBadge state={run.state} /></td>
        <td className="td px-3 text-text-dim">{run.type || '—'}</td>
        <td className="td px-3 text-text-dim text-xs whitespace-nowrap">
          <span title={started.title}>{started.text}</span>
        </td>
        <td className="td px-3 text-text-dim text-xs whitespace-nowrap">{runDuration(run.durationSeconds)}</td>
        <td className="td px-3 text-text-dim">{run.triggeredBy || '—'}</td>
        <td className="td px-3">
          {run.note ? <span className="break-words">{run.note}</span> : <span className="text-text-mute">—</span>}
        </td>
        <td className="td px-3">
          {active && (
            <button
              type="button"
              className="btn-ghost py-1 px-2 text-xs text-bad whitespace-nowrap"
              aria-label={`Stop run ${run.runId}`}
              onClick={onStop}
            >
              Stop run
            </button>
          )}
        </td>
      </tr>
      {open && (
        <tr id={panelId} className="bg-bg-soft/20">
          <td colSpan={8} className="px-4 py-4">
            {/* Sticky and no wider than the card, so the panel stays in view when the runs table scrolls sideways. */}
            <div className="sticky left-0 max-w-[calc(100cqw-2.5rem)]">
              <RunTasks namespace={pipeline.namespace} name={pipeline.name} runId={run.runId} />
            </div>
          </td>
        </tr>
      )}
    </>
  )
}

function RunTasks({ namespace, name, runId }: { namespace: string; name: string; runId: string }) {
  const [log, setLog] = useState<{ taskId: string; tryNumber: number } | null>(null)
  const tasks = useQuery({
    queryKey: ['pipeline-run', namespace, name, runId],
    queryFn: () => api.pipelineRun(namespace, name, runId),
    refetchInterval: (query) => {
      const state = query.state.data?.run.state
      return state === 'queued' || state === 'running' ? 5_000 : false
    },
  })

  if (tasks.isLoading) return <p className="text-sm text-text-mute">Loading tasks…</p>
  if (tasks.isError || !tasks.data) {
    return <ErrorBox error={tasks.error} onRetry={() => void tasks.refetch()} />
  }

  return (
    <div className="space-y-3">
      <h3 className="text-sm font-semibold break-all">
        Tasks of run <span className="font-mono text-xs">{runId}</span>
      </h3>
      {tasks.data.tasks.length === 0 ? (
        <p className="text-sm text-text-mute">This run has no tasks yet.</p>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <caption className="sr-only">Task instances of this run.</caption>
            <thead>
              <tr>
                <th scope="col" className="th min-w-[7rem] px-3 py-2">Task</th>
                <th scope="col" className="th px-3 py-2">State</th>
                <th scope="col" className="th px-3 py-2">Try</th>
                <th scope="col" className="th px-3 py-2">Started</th>
                <th scope="col" className="th px-3 py-2">Duration</th>
                <th scope="col" className="th px-3 py-2">Operator</th>
                <th scope="col" className="th px-3 py-2">
                  <span className="sr-only">Log</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {tasks.data.tasks.map((task) => {
                const started = formatWhen(task.startedAt)
                return (
                  <tr key={`${task.taskId}-${task.tryNumber}`} className="border-t border-border/70">
                    <td className="td px-3"><span className="font-mono text-xs whitespace-nowrap">{task.taskId}</span></td>
                    <td className="td px-3"><RunStateBadge state={task.state} /></td>
                    <td className="td px-3 text-text-dim">{task.tryNumber}</td>
                    <td className="td px-3 text-text-dim text-xs whitespace-nowrap">
                      <span title={started.title}>{started.text}</span>
                    </td>
                    <td className="td px-3 text-text-dim text-xs whitespace-nowrap">
                      {runDuration(task.durationSeconds)}
                    </td>
                    <td className="td px-3 text-text-dim">{task.operator ?? '—'}</td>
                    <td className="td px-3">
                      <button
                        type="button"
                        className="btn-ghost py-1 px-2 text-xs"
                        aria-label={`View log of ${task.taskId} try ${task.tryNumber}`}
                        onClick={() => setLog({ taskId: task.taskId, tryNumber: task.tryNumber })}
                      >
                        View log
                      </button>
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      )}

      {log && (
        <TaskLog
          key={`${log.taskId}-${log.tryNumber}`}
          namespace={namespace}
          name={name}
          runId={runId}
          taskId={log.taskId}
          tryNumber={log.tryNumber}
        />
      )}
    </div>
  )
}

/** A level as a word: colour is never the only signal. */
function levelWord(level: string | null): string {
  if (!level) return 'Log'
  return level.charAt(0).toUpperCase() + level.slice(1).toLowerCase()
}

function TaskLog({
  namespace,
  name,
  runId,
  taskId,
  tryNumber,
}: {
  namespace: string
  name: string
  runId: string
  taskId: string
  tryNumber: number
}) {
  const [copied, setCopied] = useState(false)
  const log = useInfiniteQuery({
    queryKey: ['pipeline-log', namespace, name, runId, taskId, tryNumber],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      api.pipelineLog(namespace, name, runId, taskId, { tryNumber, token: pageParam }),
    getNextPageParam: (page) => page.nextToken || undefined,
  })

  const pages = log.data?.pages ?? []
  const lines: LogLine[] = pages.flatMap((page) => page.lines)
  const truncated = pages.some((page) => page.truncated)

  async function copy() {
    const text = lines
      .map((line) => [line.timestamp ?? '', levelWord(line.level), line.text].filter(Boolean).join(' '))
      .join('\n')
    try {
      await navigator.clipboard.writeText(text)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 1500)
    } catch {
      // Clipboard access can be refused (insecure origin, permissions); nothing to report.
    }
  }

  return (
    <div className="rounded-[10px] border border-border bg-bg-elev/40">
      <div className="flex flex-wrap items-center gap-2 border-b border-border/70 px-3 py-2">
        <span className="min-w-0 text-sm font-medium break-all">
          Log of <span className="font-mono text-xs">{taskId}</span> · try {tryNumber}
        </span>
        <div className="ml-auto flex items-center gap-2">
          <button
            type="button"
            className="btn-ghost py-1 px-2 text-xs"
            disabled={lines.length === 0}
            onClick={() => void copy()}
          >
            {copied ? 'Copied' : 'Copy'}
          </button>
          {log.hasNextPage && (
            <button
              type="button"
              className="btn-ghost py-1 px-2 text-xs"
              disabled={log.isFetchingNextPage}
              onClick={() => void log.fetchNextPage()}
            >
              {log.isFetchingNextPage ? 'Loading…' : 'Load more'}
            </button>
          )}
        </div>
      </div>
      <div className="max-h-80 space-y-1 overflow-auto p-3">
        {log.isLoading && <p className="text-sm text-text-mute">Loading log…</p>}
        {log.isError && <ErrorBox error={log.error} onRetry={() => void log.refetch()} />}
        {log.data && lines.length === 0 && <p className="text-sm text-text-mute">No log lines (yet).</p>}
        {lines.map((line, index) => (
          <div key={index} className="flex items-start gap-2 text-xs" title={line.timestamp ?? undefined}>
            <span className="label shrink-0 pt-0.5">{levelWord(line.level)}</span>
            <span className="min-w-0 font-mono whitespace-pre-wrap break-words">{line.text}</span>
          </div>
        ))}
        {truncated && <p className="pt-1 text-xs text-text-mute">The log was cut at 2000 lines.</p>}
      </div>
    </div>
  )
}
