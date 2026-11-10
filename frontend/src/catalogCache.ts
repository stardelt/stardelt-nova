/**
 * The catalog tree's last-known lists (stardelt-nova#644).
 *
 * Opening the explorer used to wait on the catalog service before it showed anything.
 * This keeps the last answer in the browser, keyed by catalog or schema, so the tree
 * paints at once and refreshes in the background. Names are per person: `bindUser`
 * clears everything when a different person signs in, so one person's catalog names are
 * never shown to another.
 *
 * A blocked or full storage is not an error: every call here degrades to "no cache".
 */

const PREFIX = 'nova.catalog.v1:'
const OWNER = 'nova.catalog.owner'
const MAX_AGE_MS = 24 * 60 * 60 * 1000

/**
 * Whether this page load has compared the stored owner with the signed-in person.
 * Until `bindUser` has run, the entries in localStorage belong to whoever used this
 * browser last — a second person must not see the first person's names while
 * `/api/me` is still in flight (stardelt-nova#656).
 */
let ownerConfirmed = false

type Entry<T> = { at: number; data: T }

function store(): Storage | null {
  try {
    return window.localStorage
  } catch {
    return null
  }
}

/**
 * Tie the cache to the signed-in person. When the `sub` differs from the one that wrote
 * the entries, every entry is deleted first. `undefined` (dev mode, no login) is its own
 * owner, otherwise the cache could never survive a reload there.
 */
export function bindUser(sub: string | undefined): void {
  const ls = store()
  if (!ls) {
    ownerConfirmed = true
    return
  }
  try {
    const owner = sub ?? ''
    if (ls.getItem(OWNER) !== owner) {
      const stale: string[] = []
      for (let i = 0; i < ls.length; i++) {
        const key = ls.key(i)
        if (key && key.startsWith(PREFIX)) stale.push(key)
      }
      for (const key of stale) ls.removeItem(key)
    }
    ls.setItem(OWNER, owner)
    ownerConfirmed = true
  } catch {
    /* no store */
  }
}

/** Write `data` under `key` and return it, so a query function stays one expression. */
export function remember<T>(key: string, data: T): T {
  const ls = store()
  if (!ls) return data
  try {
    ls.setItem(PREFIX + key, JSON.stringify({ at: Date.now(), data } satisfies Entry<T>))
  } catch {
    /* full or blocked: the live answer is still the answer */
  }
  return data
}

/** The last answer for `key`, or `undefined` when absent or older than a day. */
export function recall<T>(key: string): T | undefined {
  if (!ownerConfirmed) return undefined
  const ls = store()
  if (!ls) return undefined
  try {
    const raw = ls.getItem(PREFIX + key)
    if (!raw) return undefined
    const entry = JSON.parse(raw) as Partial<Entry<T>> | null
    if (!entry || typeof entry.at !== 'number' || Date.now() - entry.at > MAX_AGE_MS) {
      ls.removeItem(PREFIX + key)
      return undefined
    }
    return entry.data
  } catch {
    return undefined
  }
}
