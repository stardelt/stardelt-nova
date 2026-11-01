/**
 * Image references as a human reads them.
 *
 * The cluster reports exactly what it runs — `ghcr.io/stardelt/nova@sha256:7a70…`.
 * In a card that is noise: the registry and the 64-character digest are not what
 * anyone is scanning for. Keep the name and enough of the digest to compare, and
 * put the verbatim reference in the element's `title`.
 */
export function shortImage(ref: string): string {
  if (!ref) return ''
  const at = ref.indexOf('@')
  const repo = at === -1 ? ref : ref.slice(0, at)
  const name = repo.split('/').filter(Boolean).pop() ?? repo
  if (at === -1) return name
  const digest = ref.slice(at + 1).replace(/^sha256:/, '').slice(0, 8)
  return `${name}@sha256:${digest}`
}

/**
 * An immutable object version as a person compares two of them: the first eight characters.
 * The full id is never useful in a sentence, but a truncated one distinguishes versions.
 */
export function shortVersion(id: string | null | undefined): string {
  return (id ?? '').slice(0, 8)
}

/**
 * A Git commit as a person compares two: the first seven characters, and "(uncommitted changes)"
 * when the deploy marked the working tree as dirty (`-dirty` appended to the commit).
 */
export function shortRevision(revision: string): string {
  const dirty = revision.endsWith('-dirty')
  const commit = (dirty ? revision.slice(0, -'-dirty'.length) : revision).slice(0, 7)
  return dirty ? `${commit} (uncommitted changes)` : commit
}

/**
 * Returns `raw` only if it parses as an absolute http(s) URL, else `undefined`.
 *
 * App links come from the API (derived from `NOVA_BASE_DOMAIN`). They end up in
 * `href` and iframe `src`, where a `javascript:` or `data:` value would run in
 * Nova's origin, so anything that is not plain http(s) is dropped.
 */
export function safeHttpUrl(raw: string | null | undefined): string | undefined {
  if (!raw) return undefined
  const value = raw.trim()
  try {
    const { protocol } = new URL(value)
    return protocol === 'https:' || protocol === 'http:' ? value : undefined
  } catch {
    return undefined
  }
}

/** `1 row`, `3 rows`. */
export function rowsLabel(n: number): string {
  return `${n} row${n === 1 ? '' : 's'}`
}

/** A status as a person reads it: `up` is "running", `not-installed` loses its hyphen. */
export function statusLabel(status: string): string {
  return status === 'up' ? 'running' : status === 'not-installed' ? 'not installed' : status
}

/**
 * Whether the app's `external_url` is a page a browser can open, as opposed to only
 * its address. API-only services (Lakekeeper) answer on that host with an ingress 404,
 * and Lakekeeper has no Ingress at all — an "Open" button there is a dead end.
 *
 * The backend reports this as `browsable`; a backend that predates the field sends
 * nothing, and then every app keeps its link rather than losing all of them.
 */
export function isBrowsable(app: { browsable?: boolean }): boolean {
  return app.browsable !== false
}

const PLAIN_IDENT = /^[A-Za-z_][A-Za-z0-9_]*$/

/** One SQL identifier part: left as-is when it is plain, otherwise double-quoted. */
function sqlIdent(part: string): string {
  return PLAIN_IDENT.test(part) ? part : `"${part.replace(/"/g, '""')}"`
}

/**
 * `<catalog>.<ns>.<tbl>` for generated SQL (stardelt-nova#234). Without a catalog the
 * historical `warehouse` is used, so an existing caller reads exactly as before; with one,
 * the statement names the catalog the person chose. The catalog, namespace and table come
 * from the API or from URL params, so any part that is not a plain identifier is quoted
 * instead of being pasted into the statement verbatim.
 */
export function sqlTableRef(ns: string, tbl: string, catalog = 'warehouse'): string {
  return [catalog, ...ns.split('.').filter(Boolean), tbl].map(sqlIdent).join('.')
}

/** `850 ms`, `2.3 s`, `1 m 04 s`, `1 h 05 m`; a missing value is a dash. */
export function formatDurationMs(ms: number | null | undefined): string {
  if (ms == null) return '—'
  if (ms < 1000) return `${Math.round(ms)} ms`
  const s = ms / 1000
  if (s < 60) return `${s.toFixed(s < 10 ? 1 : 0)} s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m} m ${String(Math.floor(s % 60)).padStart(2, '0')} s`
  return `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, '0')} m`
}

/** Binary units: `512 B`, `1.5 KiB`, `3.2 MiB`; a missing value is a dash. */
export function formatBytes(n: number | null | undefined): string {
  if (n == null) return '—'
  if (n < 1024) return `${n} B`
  const units = ['KiB', 'MiB', 'GiB', 'TiB']
  let v = n / 1024
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${units[i]}`
}

/** Whether full times are shown in this browser's zone or in UTC. */
export type TimeDisplay = 'local' | 'utc'

const TIME_KEY = 'stardelt-nova:time'
const TIME_EVENT = 'stardelt-nova:time-change'
let timeDisplay: TimeDisplay | null = null

/** This browser's storage, or null where there is none (tests, a blocked private window). */
function browserStorage(): Storage | null {
  try {
    return typeof window === 'undefined' ? null : window.localStorage
  } catch {
    return null
  }
}

function storedTimeDisplay(): TimeDisplay | null {
  try {
    const stored = browserStorage()?.getItem(TIME_KEY)
    if (stored === 'local' || stored === 'utc') return stored
  } catch {
    /* Private windows and blocked storage: the default applies. */
  }
  return null
}

/** Whether this browser holds a time-display choice of its own (as opposed to the default). */
export function hasTimeChoice(): boolean {
  return storedTimeDisplay() !== null
}

export function getTimeDisplay(): TimeDisplay {
  if (timeDisplay) return timeDisplay
  timeDisplay = storedTimeDisplay() ?? 'local'
  return timeDisplay
}

/** Apply a time display everywhere and remember it in this browser. */
export function setTimeDisplay(value: TimeDisplay) {
  timeDisplay = value
  try {
    browserStorage()?.setItem(TIME_KEY, value)
  } catch {
    /* ignore */
  }
  if (typeof window !== 'undefined') window.dispatchEvent(new Event(TIME_EVENT))
}

/** The browser's own abbreviation for its zone, e.g. `GMT+2`; empty when it has none. */
function zoneAbbreviation(date: Date): string {
  const parts = new Intl.DateTimeFormat(undefined, { timeZoneName: 'short' }).formatToParts(date)
  return parts.find((part) => part.type === 'timeZoneName')?.value ?? ''
}

const pad = (n: number) => String(n).padStart(2, '0')

/**
 * A full time a person can compare: `2026-10-20 14:05:09 UTC` when UTC is chosen,
 * otherwise the same instant in the browser's zone with its abbreviation. The value is
 * an epoch in milliseconds or an RFC 3339 string; anything unreadable is a dash.
 *
 * With `timeZone`, the instant is printed in that zone instead — the Schedule dialog shows
 * the next runs in the schedule's own zone, so "every day at 03:00" and its preview agree.
 * An unknown zone falls back to the person's choice.
 */
export function formatTime(value: number | string, timeZone?: string): string {
  if (value === 0 || value === '') return '—'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '—'
  if (timeZone) {
    const inZone = formatInZone(date, timeZone)
    if (inZone) return inZone
  }
  if (getTimeDisplay() === 'utc') {
    const day = `${date.getUTCFullYear()}-${pad(date.getUTCMonth() + 1)}-${pad(date.getUTCDate())}`
    const clock = `${pad(date.getUTCHours())}:${pad(date.getUTCMinutes())}:${pad(date.getUTCSeconds())}`
    return `${day} ${clock} UTC`
  }
  const day = `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`
  const clock = `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`
  const zone = zoneAbbreviation(date)
  return zone ? `${day} ${clock} ${zone}` : `${day} ${clock}`
}

/** The same instant in one named zone, with that zone's abbreviation; null for an unknown zone. */
function formatInZone(date: Date, timeZone: string): string | null {
  try {
    const parts = new Intl.DateTimeFormat('en-GB', {
      timeZone,
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      hourCycle: 'h23',
      timeZoneName: 'short',
    }).formatToParts(date)
    const part = (type: string) => parts.find((piece) => piece.type === type)?.value ?? ''
    const day = `${part('year')}-${part('month')}-${part('day')}`
    const clock = `${part('hour')}:${part('minute')}:${part('second')}`
    const zone = part('timeZoneName')
    return zone ? `${day} ${clock} ${zone}` : `${day} ${clock}`
  } catch {
    return null
  }
}

/** A count as a person reads it: `1,234`. */
export function formatCount(n: number): string {
  return n.toLocaleString()
}

/** `just now`, `5 min ago`, `3 h ago`, `2 d ago`. */
export function relativeTime(ms: number, now: number = Date.now()): string {
  const s = Math.max(0, Math.round((now - ms) / 1000))
  if (s < 45) return 'just now'
  if (s < 3600) return `${Math.max(1, Math.round(s / 60))} min ago`
  if (s < 86400) return `${Math.round(s / 3600)} h ago`
  return `${Math.round(s / 86400)} d ago`
}

/** Where a query came from, in words. Unknown clients keep the name they sent. */
export function sourceLabel(source: string | null | undefined): string {
  if (!source) return '—'
  const known: Record<string, string> = {
    'nova-sql-editor': 'SQL editor',
    nova: 'Nova',
    'stardelt-pipeline': 'Pipeline task',
    'stardelt-notebook-helper': 'Notebook',
    superset: 'Superset',
    'trino-cli': 'Trino CLI',
    'trino-jdbc': 'JDBC client',
    'trino-python-client': 'Python client',
  }
  return known[source] ?? source
}
