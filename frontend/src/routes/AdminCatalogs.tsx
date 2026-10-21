import { useEffect, useId, useMemo, useRef, useState, type FormEvent } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import {
  api,
  errorMessage,
  type AdminCatalog,
  type AdminCatalogCondition,
  type AdminCatalogGrant,
} from '../api'
import ErrorBox from '../components/ErrorBox'
import LayerBadge from '../components/LayerBadge'

// Administrator view of every catalog and its grants (stardelt-nova#251), and the one
// write (stardelt-nova#253): an administrator changes the owner, managed access and
// grants in the Edit dialog, which sends a merge patch on the Catalog resource. The
// resource stays the source of truth; the access-sync job applies the new spec to
// Lakekeeper within about two minutes, and the page polls until it reports back.
//
// There is one answer behind both views: `GET /api/admin/catalogs` holds the Catalog
// resources of the platform namespace. The page only arranges that answer two ways —
// one catalog per row (what a catalog allows and what its last sync reported), or one
// group per row (who holds what, catalog by catalog). The Matrix is read from the same
// rows, so the two views cannot disagree.

/** The two arrangements of the same rows. */
type View = 'by-catalog' | 'matrix'

/** The five verbs, in the order the dialog and the API list them. */
const VERBS = ['browse', 'select', 'write', 'create', 'manage'] as const
type Verb = (typeof VERBS)[number]

/** The list shape the API accepts: one `(group, verb)` pair per grant. */
type Grant = { group: string; verb: Verb }

/** The CRD's group rule (1–64; a leading letter or digit, then letters, digits, `. _ -`). */
const GROUP_RE = /^[A-Za-z0-9][A-Za-z0-9._-]*$/
const GROUP_SENTENCE =
  'a group name is letters, digits, dots, dashes and underscores (up to 64)'
const OWNER_SENTENCE = 'the owner is one group name'
const TOO_MANY_GRANTS = 'at most 50 grants'
const MAX_GRANTS = 50
const UNKNOWN_GROUP_SENTENCE =
  'One of the groups does not exist in the realm; nothing is granted for it.'
const WAITING_SENTENCE = 'Waiting for the sync; it runs every 2 minutes.'
const POLL_MS = 5_000
const POLL_LIMIT_MS = 3 * 60_000

/** What the page remembers about a catalog whose change is waiting for the sync. */
type Pending = { generation: number; startedAt: number }
type Outcome =
  | { kind: 'applied' }
  | { kind: 'waiting' }
  | { kind: 'alert'; message: string }

/** The conditions the access-sync job writes, in the order the page shows them. */
const CONDITION_ORDER = ['CatalogReady', 'GrantsReady', 'Drift']

/**
 * Reasons that mean the sync could not do its job. `Ready` and `Clean` are the good
 * answers; anything else the API sends makes the badge amber rather than guessing.
 */
const BAD_REASONS = new Set([
  'UnknownGroup',
  'RoleConflict',
  'RoleCreateFailed',
  'AssigneesFailed',
  'GrantsFailed',
  'ForeignGrants',
])

export default function AdminCatalogs() {
  const queryClient = useQueryClient()
  const catalogs = useQuery({ queryKey: ['admin-catalogs'], queryFn: api.adminCatalogs })
  const [view, setView] = useState<View>('by-catalog')
  const [filter, setFilter] = useState('')
  const [editing, setEditing] = useState<AdminCatalog | null>(null)
  const [pending, setPending] = useState<Record<string, Pending>>({})
  const [outcomes, setOutcomes] = useState<Record<string, Outcome>>({})

  // The API orders the rows (stardelt-nova#109): bronze, silver, gold first, then
  // alphabetically. Both views use that order, so a list the API already sorted is never
  // rearranged here.
  const rows = catalogs.data ?? []

  // Every group the page knows, for both datalists in the dialog: owners and grantees.
  const knownGroups = useMemo(() => {
    const all = new Set<string>()
    for (const catalog of rows) {
      if (catalog.owner) all.add(catalog.owner)
      for (const grant of catalog.grants) all.add(grant.group)
    }
    return [...all].sort((a, b) => a.localeCompare(b))
  }, [rows])

  const pendingCount = Object.keys(pending).length
  const refetch = catalogs.refetch

  // While a change waits for the access-sync job, ask the list every 5 s. At three
  // minutes the honest answer is that the sync has not reported back, not a spinner.
  useEffect(() => {
    if (pendingCount === 0) return
    const timer = window.setInterval(() => {
      const now = Date.now()
      const expired = Object.entries(pending)
        .filter(([, state]) => now - state.startedAt >= POLL_LIMIT_MS)
        .map(([name]) => name)
      if (expired.length > 0) {
        setPending((current) => omit(current, expired))
        setOutcomes((current) => ({
          ...current,
          ...Object.fromEntries(expired.map((name) => [name, { kind: 'waiting' as const }])),
        }))
      }
      void refetch()
    }, POLL_MS)
    return () => window.clearInterval(timer)
  }, [pending, pendingCount, refetch])

  // A change is applied when the sync ran and `observedGeneration` equals the generation
  // the save answered. GrantsReady is the sync's verdict on the grants themselves.
  useEffect(() => {
    const answer = catalogs.data
    if (!answer || pendingCount === 0) return
    const done: Record<string, Outcome> = {}
    for (const [name, state] of Object.entries(pending)) {
      const row = answer.find((catalog) => catalog.name === name)
      if (!row || row.generation !== state.generation) continue
      if (row.observedGeneration !== state.generation) continue
      const grants = row.conditions.find((condition) => condition.type === 'GrantsReady')
      if (grants?.status === 'True') {
        done[name] = { kind: 'applied' }
      } else if (grants?.reason === 'UnknownGroup') {
        done[name] = { kind: 'alert', message: UNKNOWN_GROUP_SENTENCE }
      } else {
        done[name] = {
          kind: 'alert',
          message:
            grants?.message ?? 'The access-sync job could not apply this change; its conditions show why.',
        }
      }
    }
    if (Object.keys(done).length > 0) {
      setPending((current) => omit(current, Object.keys(done)))
      setOutcomes((current) => ({ ...current, ...done }))
    }
  }, [catalogs.data, pending, pendingCount])

  /** A successful save: show the returned row at once, then wait for the sync to catch up. */
  function saved(row: AdminCatalog) {
    queryClient.setQueryData<AdminCatalog[]>(['admin-catalogs'], (current) =>
      current ? current.map((catalog) => (catalog.name === row.name ? row : catalog)) : [row],
    )
    setOutcomes((current) => omit(current, [row.name]))
    setEditing(null)
    if (typeof row.generation === 'number') {
      setPending((current) => ({
        ...current,
        [row.name]: { generation: row.generation as number, startedAt: Date.now() },
      }))
    }
  }

  return (
    <div className="max-w-6xl space-y-5">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Catalogs</h1>
        <p className="text-text-mute text-sm mt-1">
          Platform administrators can do everything in every catalog; they are not listed.
          Permissions are catalog-wide today.
        </p>
      </header>

      {catalogs.isLoading && <div className="text-text-mute text-sm">loading…</div>}
      {catalogs.isError && (
        <ErrorBox error={catalogs.error} onRetry={() => catalogs.refetch()} />
      )}

      {catalogs.isSuccess && (
        <>
          <fieldset>
            <legend className="label mb-1.5">View</legend>
            <div className="inline-flex max-w-full flex-wrap rounded-[10px] border border-border bg-bg-soft/40 p-0.5">
              <ViewChoice
                value="by-catalog"
                label="By catalog"
                checked={view === 'by-catalog'}
                onChange={() => setView('by-catalog')}
              />
              <ViewChoice
                value="matrix"
                label="Matrix"
                checked={view === 'matrix'}
                onChange={() => setView('matrix')}
              />
            </div>
          </fieldset>

          {view === 'by-catalog'
            ? (
              <ByCatalog
                rows={rows}
                pending={pending}
                outcomes={outcomes}
                onEdit={setEditing}
              />
            )
            : <Matrix rows={rows} filter={filter} onFilter={setFilter} />}
        </>
      )}

      {editing && (
        <EditCatalogDialog
          catalog={editing}
          knownGroups={knownGroups}
          onClose={() => setEditing(null)}
          onSaved={saved}
        />
      )}
    </div>
  )
}

/** A new object without the named keys; used by every pending/outcome transition. */
function omit<T>(record: Record<string, T>, keys: string[]): Record<string, T> {
  const next = { ...record }
  for (const key of keys) delete next[key]
  return next
}

/**
 * One segment of the view control. A native radio carries the choice and the keyboard
 * behaviour; the visible span is the segment, and its focus ring is the one a person sees.
 * The input keeps `focus-visible` outline utilities too, so the focused element is the
 * element with the ring rather than only its label.
 */
function ViewChoice({
  value,
  label,
  checked,
  onChange,
}: {
  value: View
  label: string
  checked: boolean
  onChange: () => void
}) {
  return (
    <label className="relative">
      <input
        type="radio"
        className="peer sr-only focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
        name="catalog-view"
        value={value}
        checked={checked}
        onChange={onChange}
      />
      <span className="inline-flex cursor-pointer items-center rounded-[8px] px-3 py-1.5 text-sm text-text-dim peer-checked:bg-accent/15 peer-checked:text-text peer-checked:shadow-sm peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-accent">
        {label}
      </span>
    </label>
  )
}

/** The catalog per row: owner, phase, the access summary and the last sync's conditions. */
function ByCatalog({
  rows,
  pending,
  outcomes,
  onEdit,
}: {
  rows: AdminCatalog[]
  pending: Record<string, Pending>
  outcomes: Record<string, Outcome>
  onEdit: (catalog: AdminCatalog) => void
}) {
  return (
    <div className="card relative overflow-x-auto">
      <table className="w-full min-w-[46rem] text-sm">
        <caption className="sr-only">All catalogs</caption>
        <thead className="bg-bg-soft/50">
          <tr>
            <th scope="col" className="th px-5 py-2.5">Catalog</th>
            <th scope="col" className="th px-3 py-2.5">Owner</th>
            <th scope="col" className="th px-3 py-2.5">Phase</th>
            <th scope="col" className="th px-3 py-2.5">Access</th>
            <th scope="col" className="th px-3 py-2.5">Conditions</th>
            <th scope="col" className="th px-3 py-2.5"><span className="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {rows.length === 0 && (
            <tr>
              <td className="px-5 py-4 text-text-mute" colSpan={6}>
                No catalogs. The cluster holds no Catalog resource in Nova&rsquo;s namespace.
              </td>
            </tr>
          )}
          {rows.map((catalog) => {
            const groups = groupedGrants(catalog)
            return (
              <tr key={catalog.name} className="border-t border-border/70">
                <td className="td px-5 py-3">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-mono font-medium break-words">{catalog.name}</span>
                    <LayerBadge layer={catalog.layer} />
                  </div>
                  {catalog.displayName && (
                    <div className="text-xs text-text-mute break-words">{catalog.displayName}</div>
                  )}
                </td>
                <td className="td px-3 py-3 break-words">
                  {catalog.owner ?? <span className="text-text-mute">—</span>}
                </td>
                <td className="td px-3 py-3">
                  <PhaseBadge catalog={catalog} />
                </td>
                <td className="td px-3 py-3">
                  <div className="flex flex-wrap items-center gap-2">
                    <span>{groups.size === 1 ? '1 group' : `${groups.size} groups`}</span>
                    {catalog.managedAccess && (
                      <span className="badge border-border bg-bg-soft text-text-dim">Managed access</span>
                    )}
                  </div>
                  {groups.size > 0 && (
                    <details className="mt-1">
                      <summary className="cursor-pointer text-xs text-accent">Grants</summary>
                      <ul className="mt-1 space-y-0.5">
                        {[...groups].map(([group, verbs]) => (
                          <li key={group} className="text-xs text-text-dim break-words">
                            <span className="font-mono">{group}</span>: {verbs.join(', ')}
                          </li>
                        ))}
                      </ul>
                    </details>
                  )}
                  <ApplyStatus pending={pending[catalog.name]} outcome={outcomes[catalog.name]} />
                </td>
                <td className="td px-3 py-3">
                  <Conditions conditions={catalog.conditions} />
                </td>
                <td className="td px-3 py-3">
                  <div className="flex flex-col items-start gap-1.5">
                    <Link
                      to={`/catalog?catalog=${encodeURIComponent(catalog.name)}`}
                      className="text-xs text-accent hover:underline whitespace-nowrap"
                      aria-label={`Open catalog ${catalog.name}`}
                    >
                      Open
                    </Link>
                    <button
                      type="button"
                      className="text-xs text-accent hover:underline whitespace-nowrap disabled:text-text-mute disabled:no-underline"
                      onClick={() => onEdit(catalog)}
                      disabled={!catalog.resourceVersion}
                      aria-label={`Edit permissions for ${catalog.name}`}
                    >
                      Edit
                    </button>
                  </div>
                </td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}

/**
 * What happened to this row's last save: the wait, the sync's verdict, or the honest
 * "not yet". An error is `role="alert"` because it is the answer to the person's click.
 */
function ApplyStatus({ pending, outcome }: { pending?: Pending; outcome?: Outcome }) {
  if (pending) {
    return (
      <p className="mt-2 text-xs text-text-mute" role="status">
        Applying…
      </p>
    )
  }
  if (!outcome) return null
  if (outcome.kind === 'applied') {
    return (
      <p className="mt-2 text-xs text-ok" role="status">
        Applied
      </p>
    )
  }
  if (outcome.kind === 'waiting') {
    return (
      <p className="mt-2 text-xs text-text-mute" role="status">
        {WAITING_SENTENCE}
      </p>
    )
  }
  return (
    <p className="mt-2 text-xs text-warn" role="alert">
      {outcome.message}
    </p>
  )
}


/**
 * One editable grant row: a group and the verbs checked for it. A `Set` would be
 * shorter, but an array keeps the order the dialog and the request use.
 */
type GrantRow = { key: number; group: string; verbs: Verb[] }

/** The r`spec.grants` array as one row per group, verbs in the dialog's order. */
function grantRows(grants: AdminCatalogGrant[]): GrantRow[] {
  const groups = new Map<string, Verb[]>()
  for (const grant of grants) {
    if (!(VERBS as readonly string[]).includes(grant.verb)) continue
    const verb = grant.verb as Verb
    const verbs = groups.get(grant.group) ?? []
    if (!verbs.includes(verb)) verbs.push(verb)
    groups.set(grant.group, verbs)
  }
  return [...groups].map(([group, verbs], index) => ({
    key: index,
    group,
    verbs: VERBS.filter((verb) => verbs.includes(verb)),
  }))
}

/** One `(group, verb)` pair per checked box, in row order then verb order. */
function grantsOf(rows: GrantRow[]): Grant[] {
  return rows.flatMap((row) => row.verbs.map((verb) => ({ group: row.group, verb })))
}

function validGroup(group: string): boolean {
  return group.length > 0 && group.length <= 64 && GROUP_RE.test(group)
}

/**
 * The Edit dialog (stardelt-nova#253). A native `<dialog>` opened with `showModal()`: the
 * browser traps focus, makes the page behind inert and closes on Escape; focus returns to
 * the Edit button. The body is exactly the PUT body: the owner, managed access, and one
 * row per group with five checkboxes. The validation sentences are the API's own, so the
 * request cannot be refused for something the form did not say first.
 *
 * Two changes are destructive enough to need a second click (`Confirm`): removing every
 * grant, and taking `stardelt-users` off `warehouse`. The resource is still the source of
 * truth — the dialog only writes `spec`; the sync job applies it.
 */
function EditCatalogDialog({
  catalog,
  knownGroups,
  onClose,
  onSaved,
}: {
  catalog: AdminCatalog
  knownGroups: string[]
  onClose: () => void
  onSaved: (row: AdminCatalog) => void
}) {
  const titleId = useId()
  const listId = useId()
  const ownerId = useId()
  const addId = useId()
  const dialog = useRef<HTMLDialogElement>(null)
  const ownerInput = useRef<HTMLInputElement>(null)
  const opener = useRef<HTMLElement | null>(null)
  const nextKey = useRef(catalog.grants.length)

  const [owner, setOwner] = useState(catalog.owner ?? '')
  const [managedAccess, setManagedAccess] = useState(catalog.managedAccess)
  const [rows, setRows] = useState<GrantRow[]>(() => grantRows(catalog.grants))
  const [newGroup, setNewGroup] = useState('')
  const [confirming, setConfirming] = useState(false)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const element = dialog.current
    if (!element) return
    opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null
    if (!element.open) element.showModal()
    ownerInput.current?.focus()
    return () => {
      if (element.open) element.close()
      opener.current?.focus()
    }
  }, [])

  const ownerValue = owner.trim()
  const grants = grantsOf(rows)
  const problems: string[] = []
  if (!ownerValue) problems.push(OWNER_SENTENCE)
  else if (!validGroup(ownerValue)) problems.push(GROUP_SENTENCE)
  if (grants.length > MAX_GRANTS) problems.push(TOO_MANY_GRANTS)
  if (rows.some((row) => !validGroup(row.group))) problems.push(GROUP_SENTENCE)
  const uniqueProblems = [...new Set(problems)]

  const removesAll = catalog.grants.length > 0 && grants.length === 0
  const removesUsers =
    catalog.name === 'warehouse' &&
    catalog.grants.some((grant) => grant.group === 'stardelt-users') &&
    !grants.some((grant) => grant.group === 'stardelt-users')
  const confirmSentences = [
    ...(removesAll ? ['This hides the catalog from people who are not administrators.'] : []),
    ...(removesUsers
      ? ['This removes access for everybody who is not in another granted group.']
      : []),
  ]

  function toggle(key: number, verb: Verb, checked: boolean) {
    setConfirming(false)
    setRows((current) =>
      current.map((row) => {
        if (row.key !== key) return row
        const verbs = checked
          ? VERBS.filter((candidate) => candidate === verb || row.verbs.includes(candidate))
          : row.verbs.filter((candidate) => candidate !== verb)
        return { ...row, verbs }
      }),
    )
  }

  function removeGroup(key: number) {
    setConfirming(false)
    setRows((current) => current.filter((row) => row.key !== key))
  }

  function addGroup() {
    const group = newGroup.trim()
    if (!group) return
    setNewGroup('')
    setConfirming(false)
    setRows((current) => {
      if (current.some((row) => row.group === group)) return current
      const row = { key: nextKey.current, group, verbs: [] as Verb[] }
      nextKey.current += 1
      return [...current, row]
    })
  }

  async function save() {
    setSaving(true)
    setError(null)
    try {
      const row = await api.putCatalogAccess(catalog.name, {
        resourceVersion: catalog.resourceVersion ?? '',
        owner: ownerValue,
        managedAccess,
        grants,
      })
      onSaved(row)
    } catch (e) {
      setSaving(false)
      setConfirming(false)
      setError(errorMessage(e))
    }
  }

  function submit(event: FormEvent) {
    event.preventDefault()
    if (saving) return
    if (uniqueProblems.length > 0) return
    if (confirmSentences.length > 0 && !confirming) {
      setConfirming(true)
      return
    }
    void save()
  }

  return (
    <dialog
      ref={dialog}
      aria-labelledby={titleId}
      onCancel={(event) => {
        event.preventDefault()
        onClose()
      }}
      className="m-auto w-[min(44rem,calc(100vw_-_2rem))] max-w-none rounded-[14px] border border-border bg-bg-elev p-0 text-text shadow-2xl backdrop:bg-black/60"
    >
      <form onSubmit={submit} className="flex max-h-[85vh] flex-col">
        <div className="px-5 pt-5">
          <h2 id={titleId} className="text-lg font-semibold break-words">
            Edit {catalog.name}
          </h2>
          <p className="mt-1 text-xs text-text-mute">
            The Catalog resource is the source of truth. The access-sync job applies this
            within about two minutes.
          </p>
        </div>

        <div className="min-h-0 space-y-4 overflow-y-auto px-5 py-4">
          <div>
            <label htmlFor={ownerId} className="label block mb-1.5">Owner</label>
            <input
              id={ownerId}
              ref={ownerInput}
              className="input w-full"
              value={owner}
              list={listId}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => setOwner(event.target.value)}
            />
            <p className="mt-1 text-xs text-text-mute">
              One group. Ownership means manage, not select.
            </p>
          </div>

          <div>
            <label className="flex items-start gap-2 text-sm">
              <input
                type="checkbox"
                className="mt-0.5 accent-accent"
                checked={managedAccess}
                onChange={(event) => setManagedAccess(event.target.checked)}
              />
              <span>Managed access</span>
            </label>
            <p className="mt-1 text-xs text-text-mute">
              The owner cannot grant on this catalog; only administrators can.
            </p>
          </div>

          <fieldset className="space-y-2">
            <legend className="label">Grants</legend>
            {rows.length === 0 && (
              <p className="text-sm text-text-mute">
                No grants. Only administrators can use this catalog.
              </p>
            )}
            {rows.map((row) => (
              <div key={row.key} className="rounded-[10px] border border-border/70 p-3">
                <div className="flex flex-wrap items-start justify-between gap-2">
                  <span className="font-mono text-sm break-words">{row.group}</span>
                  <button
                    type="button"
                    className="btn-ghost px-2 py-1 text-xs"
                    aria-label={`Remove ${row.group}`}
                    onClick={() => removeGroup(row.key)}
                  >
                    Remove
                  </button>
                </div>
                <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1">
                  {VERBS.map((verb) => (
                    <label key={verb} className="inline-flex items-center gap-1.5 text-xs">
                      <input
                        type="checkbox"
                        className="accent-accent"
                        checked={row.verbs.includes(verb)}
                        aria-label={`${verb} for ${row.group}`}
                        onChange={(event) => toggle(row.key, verb, event.target.checked)}
                      />
                      <span>{verb}</span>
                      {verb === 'manage' && (
                        <span className="text-text-mute">(grant and revoke on this catalog)</span>
                      )}
                    </label>
                  ))}
                </div>
              </div>
            ))}

            <div>
              <label htmlFor={addId} className="label block mb-1.5">Add group</label>
              <div className="flex flex-wrap gap-2">
                <input
                  id={addId}
                  className="input min-w-40 flex-1"
                  value={newGroup}
                  list={listId}
                  autoComplete="off"
                  spellCheck={false}
                  placeholder="Group name"
                  onChange={(event) => setNewGroup(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') {
                      event.preventDefault()
                      addGroup()
                    }
                  }}
                />
                <button type="button" className="btn-ghost" onClick={addGroup}>Add group</button>
              </div>
            </div>

            <datalist id={listId}>
              {knownGroups.map((group) => <option key={group} value={group} />)}
            </datalist>
          </fieldset>

          {uniqueProblems.length > 0 && (
            <div className="space-y-1">
              {uniqueProblems.map((problem) => (
                <p key={problem} className="text-xs text-warn">{problem}</p>
              ))}
            </div>
          )}

          {confirmSentences.length > 0 && confirming && (
            <div className="space-y-1 rounded-[10px] border border-warn/40 bg-warn/10 p-3">
              {confirmSentences.map((sentence) => (
                <p key={sentence} className="text-sm text-warn">{sentence}</p>
              ))}
            </div>
          )}

          {error && <p className="text-sm text-bad" role="alert">{error}</p>}
        </div>

        <div className="flex flex-wrap items-center justify-end gap-2 border-t border-border px-5 py-3">
          <button type="button" className="btn-ghost" onClick={onClose}>Cancel</button>
          <button
            type="submit"
            className="btn-primary"
            disabled={saving || uniqueProblems.length > 0}
          >
            {saving ? 'Saving…' : confirming ? 'Confirm' : 'Save'}
          </button>
        </div>
      </form>
    </dialog>
  )
}

function groupedGrants(catalog: AdminCatalog): Map<string, string[]> {
  const groups = new Map<string, string[]>()
  for (const grant of catalog.grants) {
    const verbs = groups.get(grant.group) ?? []
    if (!verbs.includes(grant.verb)) verbs.push(grant.verb)
    groups.set(grant.group, verbs)
  }
  return groups
}

function PhaseBadge({ catalog }: { catalog: AdminCatalog }) {
  const ok = catalog.phase === 'Ready'
  return (
    <span
      className={[
        'badge',
        ok
          ? 'border-emerald-400/40 bg-emerald-400/10 text-ok'
          : 'border-amber-400/40 bg-amber-400/10 text-warn',
      ].join(' ')}
      title={phaseReason(catalog)}
    >
      {catalog.phase}
    </span>
  )
}

/** The access-sync job's own words for the phase, when it wrote a condition that explains it. */
function phaseReason(catalog: AdminCatalog): string {
  const ready = catalog.conditions.find((condition) => condition.type === 'CatalogReady')
  if (ready) {
    const parts = [ready.reason, ready.message].filter((part): part is string => !!part)
    if (parts.length > 0) return parts.join(': ')
  }
  if (catalog.phase === 'Unknown') {
    return 'The access-sync job has not observed this catalog yet.'
  }
  return catalog.phase
}

/**
 * The three condition badges. The reason is the text (a person recognises
 * `UnknownGroup` faster than `False`); the sentence the job wrote is in `title`.
 */
function Conditions({ conditions }: { conditions: AdminCatalogCondition[] }) {
  const ordered = [...conditions].sort(
    (a, b) => orderOf(a.type) - orderOf(b.type),
  )
  if (ordered.length === 0) return <span className="text-text-mute">—</span>
  return (
    <div className="flex flex-wrap gap-1">
      {ordered.map((condition) => (
        <span
          key={`${condition.type}-${condition.status}-${condition.reason ?? ''}`}
          className={[
            'badge',
            conditionOk(condition)
              ? 'border-emerald-400/40 bg-emerald-400/10 text-ok'
              : 'border-amber-400/40 bg-amber-400/10 text-warn',
          ].join(' ')}
          title={condition.message ?? undefined}
        >
          {condition.type}: {condition.reason ?? condition.status}
        </span>
      ))}
    </div>
  )
}

function orderOf(type: string): number {
  const index = CONDITION_ORDER.indexOf(type)
  return index === -1 ? CONDITION_ORDER.length : index
}

/** `Clean` is Drift's good answer even though its status is `False`; a bad reason is never good. */
function conditionOk(condition: AdminCatalogCondition): boolean {
  if (condition.reason && BAD_REASONS.has(condition.reason)) return false
  return condition.status === 'True' || condition.reason === 'Clean'
}

/**
 * The same rows transposed: groups down, catalogs across. An owner is prefixed `owner`
 * because owning is a permission the resource declares outside `spec.grants`; the cell
 * still lists the verbs the group holds explicitly. Empty cells are an em dash, not a
 * blank that could be mistaken for a rendering problem.
 */
function Matrix({
  rows,
  filter,
  onFilter,
}: {
  rows: AdminCatalog[]
  filter: string
  onFilter: (value: string) => void
}) {
  const needle = filter.trim().toLowerCase()
  const groups = useMemo(() => {
    const all = new Set<string>()
    for (const catalog of rows) {
      if (catalog.owner) all.add(catalog.owner)
      for (const grant of catalog.grants) all.add(grant.group)
    }
    return [...all].sort((a, b) => a.localeCompare(b))
  }, [rows])

  const matchingGroups = needle ? groups.filter((group) => group.toLowerCase().includes(needle)) : groups
  const matchingCatalogs = needle
    ? rows.filter(
        (catalog) =>
          catalog.name.toLowerCase().includes(needle) ||
          (catalog.displayName ?? '').toLowerCase().includes(needle),
      )
    : rows

  const shownGroups = needle && matchingGroups.length > 0 ? matchingGroups : groups
  const shownCatalogs = needle && matchingCatalogs.length > 0 ? matchingCatalogs : rows
  // A needle that names neither a group nor a catalog matches nothing; the table would
  // otherwise show every row unchanged, which reads as if the filter were ignored.
  const nothingMatches = needle !== '' && matchingGroups.length === 0 && matchingCatalogs.length === 0

  return (
    <div className="space-y-3">
      <div>
        <label htmlFor="catalog-matrix-filter" className="label block mb-1.5">
          Filter groups or catalogs
        </label>
        <input
          id="catalog-matrix-filter"
          className="input w-full sm:w-72"
          value={filter}
          onChange={(event) => onFilter(event.target.value)}
          placeholder="finops, finance, …"
        />
      </div>

      <div className="card relative overflow-x-auto">
        {rows.length === 0 || nothingMatches || shownCatalogs.length === 0 || shownGroups.length === 0 ? (
          <p className="p-5 text-sm text-text-mute">
            {rows.length === 0
              ? 'No catalogs to arrange.'
              : 'No group or catalog matches that filter.'}
          </p>
        ) : (
          <table className="w-full min-w-[36rem] text-sm">
            <caption className="sr-only">Who holds what</caption>
            <thead className="bg-bg-soft/50">
              <tr>
                <th scope="col" className="th px-5 py-2.5">Group</th>
                {shownCatalogs.map((catalog) => (
                  <th key={catalog.name} scope="col" className="th px-3 py-2.5">
                    <span className="block break-words">{catalog.name}</span>
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {shownGroups.map((group) => (
                <tr key={group} className="border-t border-border/70">
                  <th scope="row" className="px-5 py-2 text-left text-xs font-normal font-mono text-text-dim break-words">
                    {group}
                  </th>
                  {shownCatalogs.map((catalog) => {
                    const parts = cellFor(catalog, group)
                    return (
                      <td key={catalog.name} className="td px-3 py-2 text-text-dim break-words">
                        {parts.length > 0 ? parts.join(', ') : <span className="text-text-mute">—</span>}
                      </td>
                    )
                  })}
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  )
}

/** What one group holds on one catalog, owner first, verbs in the order the resource declares them. */
function cellFor(catalog: AdminCatalog, group: string): string[] {
  const parts: string[] = []
  if (catalog.owner === group) parts.push('owner')
  for (const grant of catalog.grants) {
    if (grant.group === group && !parts.includes(grant.verb)) parts.push(grant.verb)
  }
  return parts
}
