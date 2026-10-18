import type { WorkloadSnapshot } from '../api'

/**
 * What Nova knows about the cluster, when it is not simply "fine".
 *
 * A workload list can be empty because nothing runs — or because the API server never
 * answered the question. Those are different facts and the backend says which one it is
 * (`state`); this turns that into a sentence, so a silent cluster shows as a quiet notice
 * instead of an empty table that reads as "nothing is running" (issue #39).
 */
export function ClusterNotice({ snapshot }: { snapshot?: WorkloadSnapshot }) {
  if (!snapshot || snapshot.state === 'ok') return null

  const stale = snapshot.state === 'stale'
  return (
    <div className="card border-amber-400/40 bg-amber-400/5 px-4 py-3 text-sm">
      <div className="flex items-start gap-3">
        <span className="badge shrink-0 border-amber-400/40 bg-amber-400/10 text-warn">
          {stale ? 'last known state' : 'cluster unreachable'}
        </span>
        <div className="min-w-0">
          <div>
            {stale
              ? `The cluster did not answer — showing the last known state, ${age(snapshot.age_seconds)}.`
              : 'Nova cannot read the cluster, so it has nothing to show here.'}
          </div>
          {snapshot.error && (
            <div className="mt-2 text-[11px] font-mono text-text-dim break-all">{snapshot.error}</div>
          )}
          <div className="text-text-mute mt-2 text-xs">
            {stale
              ? "Nova keeps asking, and drops this notice as soon as the API server answers again."
              : "That is about Nova's view, not the platform — the workloads may well be running."}
          </div>
        </div>
      </div>
    </div>
  )
}

/** `42s old` / `3m old` — how long ago the last successful read was. */
function age(seconds: number | null): string {
  if (seconds === null) return 'from an earlier read'
  if (seconds < 5) return 'from a moment ago'
  if (seconds < 90) return `${seconds}s old`
  return `${Math.round(seconds / 60)}m old`
}
