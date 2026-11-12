/**
 * The Overview's small charts (stardelt-nova#767, slice 2 of #510): counting, no drawing.
 * Pure functions only — no React, no dependency, no `Date.now()` of their own, so the caller
 * owns the window and a test can pin it. Days are the person's local days: the page already
 * formats times in their zone.
 */

/** Midnight of the local day that contains `ms`, as epoch milliseconds. */
export function dayStart(ms: number): number {
  const d = new Date(ms)
  d.setHours(0, 0, 0, 0)
  return d.getTime()
}

/** The oldest timestamp in a list, or null when nothing has one. */
export function oldestOf(times: (number | null | undefined)[]): number | null {
  let oldest: number | null = null
  for (const at of times) {
    if (typeof at !== 'number' || !Number.isFinite(at)) continue
    if (oldest === null || at < oldest) oldest = at
  }
  return oldest
}

/**
 * Where a chart window starts when the answer may be truncated: the index of the oldest covered
 * day, so a day the cap cut off is absent instead of drawn as zero. `0` when nothing is truncated.
 */
export function coveredStart(days: number[], oldest: number | null, truncated: boolean): number {
  if (!truncated || oldest === null) return 0
  const index = days.findIndex((day) => day >= dayStart(oldest))
  return index < 0 ? days.length : index
}

/** The midnights of the last `days` local days, oldest first, today's last. */
export function dayStarts(days: number, now: number): number[] {
  const count = Number.isFinite(days) ? Math.floor(days) : 0
  if (count <= 0 || !Number.isFinite(now)) return []
  // Walk calendar days rather than subtracting 86_400_000: a local day is 23 or 25 hours
  // long twice a year, and an arithmetic window would drift off midnight.
  const today = new Date(dayStart(now))
  const out: number[] = []
  for (let back = count - 1; back >= 0; back--) {
    const day = new Date(today)
    day.setDate(day.getDate() - back)
    day.setHours(0, 0, 0, 0)
    out.push(day.getTime())
  }
  return out
}

/** One bar of the runs chart: how many runs of each state started that day. */
export type RunBucket = { ok: number; failed: number; running: number; other: number }

/** The window `dayStarts(days, now)` describes: `[start, end)` in epoch milliseconds. */
function chartWindow(days: number, now: number): { starts: number[]; end: number } {
  const starts = dayStarts(days, now)
  if (starts.length === 0) return { starts, end: 0 }
  const after = new Date(starts[starts.length - 1])
  after.setDate(after.getDate() + 1)
  after.setHours(0, 0, 0, 0)
  return { starts, end: after.getTime() }
}

/** The index of the day `t` falls in, or `-1` outside the window (before it, or at/after tomorrow). */
function bucketOf(t: number, starts: number[], end: number): number {
  for (let i = 0; i < starts.length; i++) {
    const next = i + 1 < starts.length ? starts[i + 1] : end
    if (t >= starts[i] && t < next) return i
  }
  return -1
}

/**
 * `success` counts as ok; `failed` and `upstream_failed` as failed; `queued` and `running` as
 * running; every other state as other. A run whose time does not parse is dropped, never
 * bucketed — an `Invalid Date` must never reach the DOM.
 */
export function runsPerDay(
  runs: { state: string; startedAt?: string | null; logicalDate?: string | null }[],
  days: number,
  now: number,
): RunBucket[] {
  const { starts, end } = chartWindow(days, now)
  const buckets: RunBucket[] = starts.map(() => ({ ok: 0, failed: 0, running: 0, other: 0 }))
  if (starts.length === 0) return buckets
  for (const run of runs) {
    const t = Date.parse(run.startedAt ?? run.logicalDate ?? '')
    if (!Number.isFinite(t)) continue
    const i = bucketOf(t, starts, end)
    if (i < 0) continue
    switch ((run.state ?? '').toLowerCase()) {
      case 'success':
        buckets[i].ok++
        break
      case 'failed':
      case 'upstream_failed':
        buckets[i].failed++
        break
      case 'queued':
      case 'running':
        buckets[i].running++
        break
      default:
        buckets[i].other++
        break
    }
  }
  return buckets
}

/** Query counts per day; timestamps outside the window are ignored. */
export function queriesPerDay(times: number[], days: number, now: number): number[] {
  const { starts, end } = chartWindow(days, now)
  const counts = starts.map(() => 0)
  if (starts.length === 0) return counts
  for (const t of times) {
    if (!Number.isFinite(t)) continue
    const i = bucketOf(t, starts, end)
    if (i >= 0) counts[i]++
  }
  return counts
}
