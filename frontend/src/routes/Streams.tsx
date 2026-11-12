import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, errorMessage, type StreamView } from '../api'
import { EmptyState } from '../components/EmptyState'
import ErrorBox from '../components/ErrorBox'
import { formatWhen } from './Pipelines'

const NEUTRAL = 'border-border bg-bg-soft text-text-dim'
const GOOD = 'border-emerald-400/40 bg-emerald-400/10 text-ok'
const BAD = 'border-red-400/40 bg-red-400/10 text-bad'
const BUSY = 'border-sky-400/40 bg-sky-400/10 text-info'

type PhaseLook = { word: string; cls: string }

/** The phase words of `StreamView.phase`; the word is shown as-is for unknown values. */
const PHASES: Record<string, PhaseLook> = {
  Running: { word: 'Running', cls: GOOD },
  Stopped: { word: 'Stopped', cls: NEUTRAL },
  Failed: { word: 'Failed', cls: BAD },
  Pending: { word: 'Pending', cls: BUSY },
}

/** The operator's own window: `Restarting` lasts ten minutes after a submission. */
const RESTART_WINDOW_MS = 10 * 60 * 1000

/** True when the last submission is inside the operator's restart window. */
function restartedRecently(iso: string | null): boolean {
  if (!iso) return false
  const then = Date.parse(iso)
  return !Number.isNaN(then) && Date.now() - then < RESTART_WINDOW_MS
}

function PhaseBadge({ phase, reason }: { phase: string; reason: string | null }) {
  const look = PHASES[phase] ?? { word: phase || 'Unknown', cls: NEUTRAL }
  return (
    <span className="inline-flex items-center gap-1">
      <span className={`badge ${look.cls}`}>{look.word}</span>
      {reason && reason !== phase && <span className={`badge ${BUSY}`}>{reason}</span>}
    </span>
  )
}

export default function Streams() {
  const qc = useQueryClient()
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  // Demo mode has no cluster, so the patch route stays 503 (#500): do not offer the action.
  const demo = me.data?.demo === true
  const streams = useQuery({ queryKey: ['streams'], queryFn: api.streams, refetchInterval: 15_000 })
  const mutation = useMutation({
    mutationFn: ({ s, stopped }: { s: StreamView; stopped: boolean }) =>
      api.setStreamState(s.namespace, s.name, stopped),
    onMutate: async ({ s, stopped }) => {
      await qc.cancelQueries({ queryKey: ['streams'] })
      const previous = qc.getQueryData<{ streams: StreamView[] }>(['streams'])
      qc.setQueryData<{ streams: StreamView[] }>(['streams'], (old) =>
        old
          ? {
              ...old,
              streams: old.streams.map((row) =>
                row.namespace === s.namespace && row.name === s.name
                  ? { ...row, stopped, phase: stopped ? 'Stopped' : 'Running' }
                  : row,
              ),
            }
          : old,
      )
      return { previous }
    },
    onError: (_error, _vars, context) => {
      if (context?.previous) qc.setQueryData(['streams'], context.previous)
    },
    onSettled: () => qc.invalidateQueries({ queryKey: ['streams'] }),
  })

  return (
    <div className="flex flex-col gap-4">
      <div>
        <h1 className="text-2xl font-bold tracking-tight">Streams</h1>
        <p className="text-sm text-text-dim">
          Jobs that read a stream topic and write it into a table, without stopping.
        </p>
      </div>

      {streams.isError && <ErrorBox error={streams.error} onRetry={() => streams.refetch()} />}

      {streams.data && streams.data.streams.length === 0 && (
        <EmptyState icon="spark" title="No streams yet">
          A stream is created as a <span className="font-mono">StreamApp</span> resource; ask an administrator to add one.
        </EmptyState>
      )}

      {streams.data && streams.data.streams.length > 0 && (
        <>
          {mutation.isError && (
            <p role="alert" className="text-sm text-bad">
              {errorMessage(mutation.error)}
            </p>
          )}
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border text-left text-text-dim">
                  <th className="py-2 pr-3 font-medium">Name</th>
                  <th className="py-2 pr-3 font-medium">Phase</th>
                  <th className="py-2 pr-3 font-medium">Source</th>
                  <th className="py-2 pr-3 font-medium">Sink</th>
                  <th className="py-2 pr-3 font-medium" title="The newest checkpoint commit, as the operator reports it">Checkpoint</th>
                  <th className="py-2 pr-3 font-medium">Last error</th>
                  {!demo && <th className="py-2 font-medium">Job</th>}
                </tr>
              </thead>
              <tbody>
                {streams.data.streams.map((s) => (
                  <tr key={s.namespace + '/' + s.name} className="border-b border-border/60 align-top">
                    <td className="py-2 pr-3">
                      <span className="font-mono">{s.name}</span>
                      <div className="text-text-dim text-xs">{s.owner_group}</div>
                    </td>
                    <td className="py-2 pr-3">
                      <PhaseBadge phase={s.phase} reason={s.reason} />
                      {typeof s.attempts === 'number' && s.attempts > 1 && (
                        <span
                          className={`badge ${restartedRecently(s.last_submitted_at) ? BUSY : NEUTRAL} ml-1`}
                          title={
                            s.last_submitted_at
                              ? `last submitted ${formatWhen(s.last_submitted_at).text}`
                              : `${s.attempts} submissions since the stream was created`
                          }
                        >
                          {s.attempts}× {restartedRecently(s.last_submitted_at) ? 'restarted' : 'submissions'}
                        </span>
                      )}
                    </td>
                    <td className="py-2 pr-3">
                      {s.source_connection} · {s.source_topic}
                    </td>
                    <td className="py-2 pr-3">
                      <div>
                        {s.sink_catalog}.{s.sink_table}
                      </div>
                      <div className="text-xs text-text-dim">{s.sink_mode}</div>
                    </td>
                    <td className="py-2 pr-3">
                      {s.last_checkpoint_at ? (
                        <span title={formatWhen(s.last_checkpoint_at).title}>
                          {formatWhen(s.last_checkpoint_at).text}
                        </span>
                      ) : (
                        <span
                          className="text-text-dim text-xs"
                          title="The operator does not report a checkpoint time yet; the checkpoint itself is written to the object store and survives a stop."
                        >
                          not reported yet
                        </span>
                      )}
                    </td>
                    <td className="py-2 pr-3 text-bad text-xs break-words">{s.last_error ?? '—'}</td>
                    {!demo && (
                      <td className="py-2">
                        <button
                          type="button"
                          aria-label={`${s.stopped ? 'Resume' : 'Stop'} ${s.name}`}
                          disabled={mutation.isPending && mutation.variables?.s.name === s.name}
                          onClick={() => mutation.mutate({ s, stopped: !s.stopped })}
                          className="rounded border border-border bg-bg-soft px-2 py-1 text-xs text-text hover:bg-bg disabled:opacity-50"
                        >
                          {s.stopped ? 'Resume' : 'Stop'}
                        </button>
                      </td>
                    )}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}
    </div>
  )
}
