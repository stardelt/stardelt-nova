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
