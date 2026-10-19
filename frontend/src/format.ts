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
 * `warehouse.<ns>.<tbl>` for generated SQL. The namespace and table come from
 * the catalog (or from URL params), so a part that is not a plain identifier is
 * quoted instead of being pasted into the statement verbatim. Plain names are
 * unchanged, so the common case reads exactly as before.
 */
export function sqlTableRef(ns: string, tbl: string): string {
  return ['warehouse', ...ns.split('.').filter(Boolean), tbl].map(sqlIdent).join('.')
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
    superset: 'Superset',
    'trino-cli': 'Trino CLI',
    'trino-jdbc': 'JDBC client',
    'trino-python-client': 'Python client',
  }
  return known[source] ?? source
}
