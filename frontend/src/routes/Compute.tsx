import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, errorMessage, type ComputeCluster } from '../api'
import { EmptyState } from '../components/EmptyState'
import ErrorBox from '../components/ErrorBox'
import { formatWhen } from './Pipelines'

const NEUTRAL = 'border-border bg-bg-soft text-text-dim'
const GOOD = 'border-emerald-400/40 bg-emerald-400/10 text-ok'
const BAD = 'border-red-400/40 bg-red-400/10 text-bad'
const BUSY = 'border-sky-400/40 bg-sky-400/10 text-info'

type PhaseLook = { word: string; cls: string }

/** The phase words of `ComputeCluster.phase`; the word is shown as-is for unknown values. */
const PHASES: Record<string, PhaseLook> = {
  Starting: { word: 'Starting', cls: BUSY },
  Running: { word: 'Running', cls: GOOD },
  Stopping: { word: 'Stopping', cls: BUSY },
  Stopped: { word: 'Stopped', cls: NEUTRAL },
  Failed: { word: 'Failed', cls: BAD },
}

/** Who may use a cluster: its group, else its person. */
function audience(owner: ComputeCluster['owner']): string {
  return owner.group ? owner.group : owner.user ?? '—'
}

export default function Compute() {
  const qc = useQueryClient()
  const clusters = useQuery({ queryKey: ['compute'], queryFn: api.compute, refetchInterval: 15_000 })
  const start = useMutation({
    mutationFn: (name: string) => api.startCompute(name),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['compute'] }),
  })
  const stop = useMutation({
    mutationFn: (name: string) => api.stopCompute(name),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['compute'] }),
  })
  const pending = start.isPending || stop.isPending

  return (
    <div className="flex flex-col gap-4">
      <div>
        <h1 className="text-2xl font-bold tracking-tight">Compute</h1>
        <p className="text-sm text-text-dim">
          Spark clusters a team starts when it needs them; the shared cluster is always on.
        </p>
      </div>

      {clusters.isError && <ErrorBox error={clusters.error} onRetry={() => clusters.refetch()} />}

      {clusters.data && clusters.data.clusters.length === 0 && (
        <EmptyState icon="spark" title="No compute clusters">
          A cluster is created as a <span className="font-mono">SparkCluster</span> resource; ask an administrator to
          create one.
        </EmptyState>
      )}

      {clusters.data && clusters.data.clusters.length > 0 && (
        <>
          {(start.isError || stop.isError) && (
            <p role="alert" className="text-sm text-bad">
              {errorMessage((start.error ?? stop.error) as unknown)}
            </p>
          )}
          <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
            {clusters.data.clusters.map((c) => {
              const look = PHASES[c.phase] ?? { word: c.phase || 'Unknown', cls: NEUTRAL }
              const since = c.startedAt && c.phase === 'Running' ? formatWhen(c.startedAt).text : null
              return (
                <article key={c.name} className="flex flex-col gap-2 rounded-[10px] border border-border bg-bg-soft/40 p-3">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-medium break-words">{c.displayName ?? c.name}</span>
                    <span className={`badge ${look.cls}`}>{look.word}</span>
                  </div>
                  <div className="flex flex-wrap items-center gap-2 text-xs text-text-dim">
                    <span>{audience(c.owner)}</span>
                    <span className={`badge ${NEUTRAL}`}>{c.size}</span>
                    <span className="font-mono text-xs">{c.runtime}</span>
                  </div>
                  {c.shared ? (
                    <p className="text-sm text-text-dim">Shared · always on · for everyone</p>
                  ) : (
                    <>
                      <p className="text-sm text-text-dim">
                        {since ? `Running since ${since} · ` : ''}Stops after {c.idleStopMinutes} min idle
                      </p>
                      <div className="flex gap-2">
                        {(c.phase === 'Stopped' || c.phase === 'Failed') && (
                          <button
                            type="button"
                            className="btn-ghost px-2.5 py-1 text-xs"
                            disabled={pending}
                            onClick={() => start.mutate(c.name)}
                          >
                            Start
                          </button>
                        )}
                        {c.phase === 'Running' && (
                          <button
                            type="button"
                            className="btn-ghost px-2.5 py-1 text-xs"
                            disabled={pending}
                            onClick={() => stop.mutate(c.name)}
                          >
                            Stop
                          </button>
                        )}
                      </div>
                    </>
                  )}
                </article>
              )
            })}
          </div>
        </>
      )}
    </div>
  )
}
