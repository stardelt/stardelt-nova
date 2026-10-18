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
