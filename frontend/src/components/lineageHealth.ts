import { relativeTime } from '../format'

/** The newest run of the pipeline that wrote a table (nova#646 L6b). */
export type RunHealth = { state: string; startedAt: string | null }

/**
 * The dot on a table: the newest run of the pipeline that wrote it. A state a person cannot
 * act on (`skipped`, an unknown word) is no dot at all, so the table reads as it always did.
 */
export function runDot(run: RunHealth | undefined): { paint: string; title: string } | null {
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
