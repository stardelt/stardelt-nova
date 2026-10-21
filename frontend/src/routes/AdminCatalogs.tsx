import { useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api, type AdminCatalog, type AdminCatalogCondition } from '../api'
import ErrorBox from '../components/ErrorBox'

// Administrator view of every catalog and its grants (stardelt-nova#251, read-only half).
//
// There is one answer behind both views: `GET /api/admin/catalogs` holds the Catalog
// resources of the platform namespace, and the resource stays the source of truth. The
// page only arranges that answer two ways — one catalog per row (what a catalog allows
// and what its last sync reported), or one group per row (who holds what, catalog by
// catalog). The Matrix is read from the same rows, so the two views cannot disagree.

/** The two arrangements of the same rows. */
type View = 'by-catalog' | 'matrix'

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
  const catalogs = useQuery({ queryKey: ['admin-catalogs'], queryFn: api.adminCatalogs })
  const [view, setView] = useState<View>('by-catalog')
  const [filter, setFilter] = useState('')

  // The API sorts by name; sorting here as well keeps the page honest if the answer ever
  // arrives in another order, and both views use the same order.
  const rows = useMemo(
    () => [...(catalogs.data ?? [])].sort((a, b) => a.name.localeCompare(b.name)),
    [catalogs.data],
  )

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
            ? <ByCatalog rows={rows} />
            : <Matrix rows={rows} filter={filter} onFilter={setFilter} />}
        </>
      )}
    </div>
  )
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
function ByCatalog({ rows }: { rows: AdminCatalog[] }) {
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
            <th scope="col" className="th px-3 py-2.5"><span className="sr-only">Open</span></th>
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
                  <div className="font-mono font-medium break-words">{catalog.name}</div>
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
                </td>
                <td className="td px-3 py-3">
                  <Conditions conditions={catalog.conditions} />
                </td>
                <td className="td px-3 py-3">
                  <Link
                    to={`/catalog?catalog=${encodeURIComponent(catalog.name)}`}
                    className="text-xs text-accent hover:underline whitespace-nowrap"
                    aria-label={`Open catalog ${catalog.name}`}
                  >
                    Open
                  </Link>
                </td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}

/** `N groups` is the number of groups, not the number of grants: a group may hold several verbs. */
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
