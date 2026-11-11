import { useEffect, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api, errorMessage, type WorkspaceView } from '../api'
import { getTimeDisplay } from '../format'

// "My workspace" (stardelt-nova#653): the state of the caller's own JupyterHub server, and
// the two things a person may do about it — start it, stop it. Opening it is a plain link to
// the hub: Nova has no `access:servers` scope and can never open a server itself. Starting it
// is the hub's `/hub/spawn` route for the same reason: only the hub can renew its own stored
// sign-in for the person (nova#808).

type State = WorkspaceView['state']

type Look = { word: string; dot: string; ink: string }

/** The status dot and word per state. `starting` and `stopping` pulse. */
const LOOKS: Record<State, Look> = {
  running: { word: 'running', dot: 'bg-ok', ink: 'text-ok' },
  starting: { word: 'starting', dot: 'bg-info animate-pulse', ink: 'text-info' },
  stopping: { word: 'stopping', dot: 'bg-info animate-pulse', ink: 'text-info' },
  stopped: { word: 'stopped', dot: 'border border-text-mute', ink: 'text-text-dim' },
  unavailable: { word: 'unavailable', dot: 'hidden', ink: 'text-text-mute' },
}

const KNOWN: State[] = ['running', 'starting', 'stopping', 'stopped']

/** The state of an answer, tolerating a shape this build does not know (`unavailable`). */
function stateOf(view: WorkspaceView | undefined): State {
  const state = view?.state
  return state && KNOWN.includes(state) ? state : 'unavailable'
}

const pad = (n: number) => String(n).padStart(2, '0')

/** `09:12` — the hour a server started, in the zone the person chose for times. */
function clockOf(iso: string | null | undefined): string | null {
  if (!iso) return null
  const date = new Date(iso)
  if (Number.isNaN(date.getTime())) return null
  const utc = getTimeDisplay() === 'utc'
  return `${pad(utc ? date.getUTCHours() : date.getHours())}:${pad(utc ? date.getUTCMinutes() : date.getMinutes())}`
}

/** `in 47 min`, when the hub culls an idle server; `now` once that time has come. */
function untilLabel(iso: string | null | undefined, now: number): string | null {
  if (!iso) return null
  const at = new Date(iso).getTime()
  if (Number.isNaN(at)) return null
  const seconds = Math.round((at - now) / 1000)
  if (seconds <= 0) return 'any moment now'
  if (seconds < 90) return `in ${seconds} s`
  const minutes = Math.round(seconds / 60)
  if (minutes < 60) return `in ${minutes} min`
  const hours = Math.floor(minutes / 60)
  return `in ${hours} h ${pad(minutes % 60)} min`
}

/** `40 s` since the hub was asked to spawn the server. */
function secondsLabel(seconds: number): string {
  if (seconds < 60) return `${seconds} s`
  return `${Math.floor(seconds / 60)} min ${pad(seconds % 60)} s`
}

export default function WorkspaceCard({ open, spawn }: { open?: string; spawn?: string }) {
  const qc = useQueryClient()
  const workspace = useQuery({
    queryKey: ['workspace'],
    queryFn: api.workspace,
    // A server that is coming up (or going down) changes within seconds; a settled one
    // only when its idle stop approaches.
    refetchInterval: (query) =>
      query.state.data?.state === 'starting' || query.state.data?.state === 'stopping' ? 3000 : 30_000,
  })
  const start = useMutation({
    mutationFn: api.startWorkspace,
    onSuccess: () => qc.invalidateQueries({ queryKey: ['workspace'] }),
  })
  const stop = useMutation({
    mutationFn: api.stopWorkspace,
    onSuccess: () => qc.invalidateQueries({ queryKey: ['workspace'] }),
  })
  const pending = start.isPending || stop.isPending
  const state = stateOf(workspace.data)

  // When this page first saw the server come up. The hub reports `started` only once it is
  // ready, so the elapsed seconds of a spawn are counted here.
  const [since, setSince] = useState<number | null>(null)
  useEffect(() => {
    setSince(state === 'starting' ? Date.now() : null)
  }, [state])
  // One tick a second while counting (and while the idle stop countdown is shown).
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (state !== 'starting' && state !== 'running') return
    setNow(Date.now())
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [state])
  const elapsed = since == null ? 0 : Math.max(0, Math.floor((now - since) / 1000))

  // Nothing the hub reports: the page keeps today's way in and nothing else.
  if (state === 'unavailable') {
    if (!open) return null
    return (
      <section aria-label="My workspace" className="card flex flex-wrap items-center justify-between gap-3 px-5 py-4">
        <p className="text-sm text-text-dim">Your workspace is not reported by this installation.</p>
        <div className="flex items-center gap-2">
          <a href={open} target="_blank" rel="noreferrer" className="btn-primary py-1.5">
            Open JupyterLab ↗
          </a>
          <Link to="/profile" className="btn-ghost py-1.5 text-xs">VS Code and tokens</Link>
        </div>
      </section>
    )
  }

  const look = LOOKS[state]
  const started = clockOf(workspace.data?.startedAt)
  const until = untilLabel(workspace.data?.idleStopAt, now)
  const error = start.isError || stop.isError ? errorMessage(start.error ?? stop.error) : null

  return (
    <section aria-label="My workspace" className="card px-5 py-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h2 className="text-sm font-semibold">My workspace</h2>
          <p className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-sm">
            <span className={`inline-flex items-center gap-1.5 ${look.ink}`}>
              <span aria-hidden="true" className={`h-2 w-2 rounded-full ${look.dot}`} />
              {look.word}
            </span>
            {state === 'starting' && (
              <span className="text-text-dim">· {secondsLabel(elapsed)}</span>
            )}
            {state === 'running' && until && (
              <span className="text-text-dim">· idle stop {until}</span>
            )}
            {state === 'running' && started && (
              <span className="text-text-dim">· started {started}</span>
            )}
            {state === 'stopped' && (
              <span className="text-text-dim">· start it when you need it</span>
            )}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {open && (
            <a
              href={open}
              target="_blank"
              rel="noreferrer"
              className={state === 'running' ? 'btn-primary py-1.5' : 'btn-ghost py-1.5 text-xs'}
            >
              Open JupyterLab ↗
            </a>
          )}
          {state === 'stopped' && (
            // The hub's own spawn route, not `POST /api/workspace/start`: only the hub's login
            // can renew a stored OAuth token that has aged out, and the spawn route does that
            // through the SSO session the visitor already has (nova#808).
            spawn ? (
              <a href={spawn} target="_blank" rel="noreferrer" className="btn-primary py-1.5">
                Start
              </a>
            ) : (
              <button type="button" className="btn-primary py-1.5" disabled={pending} onClick={() => start.mutate()}>
                Start
              </button>
            )
          )}
          {(state === 'running' || state === 'starting') && (
            <button type="button" className="btn-ghost py-1.5 text-xs" disabled={pending} onClick={() => stop.mutate()}>
              Stop
            </button>
          )}
          <Link to="/profile" className="btn-ghost py-1.5 text-xs">VS Code and tokens</Link>
        </div>
      </div>
      {error && (
        <p role="alert" className="mt-2 text-sm text-bad">
          {error}
        </p>
      )}
    </section>
  )
}
