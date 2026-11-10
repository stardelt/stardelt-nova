import { useQuery } from '@tanstack/react-query'
import { Link, useSearchParams } from 'react-router-dom'
import { ApiError, api, type Catalog } from '../api'
import ErrorBox from './ErrorBox'
import LayerBadge from './LayerBadge'
import ObjectIcon from './ObjectIcon'
import { relativeTime } from '../format'

/** A 403 says no: the person's grants do not change by asking again. */
function forbidden(error: unknown): boolean {
  if (error instanceof ApiError) return error.status === 403
  return !!error && typeof error === 'object' && 'status' in error && (error as { status?: unknown }).status === 403
}

/** The verbs a group reads with, and the ones it writes with (`VERBS` in `AdminCatalogs.tsx`
 * is the full set). */
const READ_VERBS = ['browse', 'select']
const WRITE_VERBS = ['write', 'create', 'manage']

/** "2 h ago · 3 tables optimized" / "· 1 failed" / "—". */
function maintenanceLine(m?: Catalog['maintenance']): string {
  if (!m?.lastRun) return '—'
  const when = relativeTime(Date.parse(m.lastRun))
  const done = m.tablesDone != null ? `${m.tablesDone} tables optimized` : 'run finished'
  const failed = m.tablesFailed ? ` · ${m.tablesFailed} failed` : ''
  return `${when} · ${done}${failed}`
}

/**
 * The right pane's page for a catalog (stardelt-nova#584): what the catalog is and who may
 * use it, then its schemas. The left pane is navigation only, so the description, the owner
 * and the grants belong here, next to the object they describe.
 *
 * The owner, the description and the grants are the catalog's `Catalog` resource (the API
 * sends them with the catalog list). A person who may not write is told whom to ask only
 * when there is an owner to ask.
 */
export default function CatalogOverview({ catalog }: { catalog: Catalog }) {
  const [, setParams] = useSearchParams()
  const grants = catalog.grants ?? []
  const groupsWith = (verbs: string[]) =>
    [...new Set(grants.filter((grant) => verbs.includes(grant.verb)).map((grant) => grant.group))]
  const read = groupsWith(READ_VERBS)
  const write = groupsWith(WRITE_VERBS)
  const owner = catalog.owner || undefined
  const description = catalog.description || undefined
  const canWrite = catalog.canWrite === true

  const namespaces = useQuery({
    queryKey: ['catalog', 'namespaces', catalog.name],
    queryFn: () => api.namespaces(catalog.name),
  })
  const schemas = namespaces.data?.namespaces ?? []

  /** Opens a schema. The tree reads the same params; the schema page follows in its own ticket. */
  const openSchema = (schema: string) => {
    const next = new URLSearchParams({ catalog: catalog.name, ns: schema })
    setParams(next)
  }

  return (
    <div className="card p-5 space-y-5">
      <header className="space-y-2">
        <h1 className="text-xl font-semibold flex items-center gap-2 flex-wrap">
          <ObjectIcon kind="catalog" />
          <span className="font-mono break-words">{catalog.name}</span>
          <LayerBadge layer={catalog.layer} />
        </h1>
        {description ? (
          <p className="text-text-dim break-words">{description}</p>
        ) : (
          <p className="text-text-mute text-sm">No description yet.</p>
        )}
      </header>

      <dl className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-5 gap-x-6 gap-y-3 text-sm">
        <div>
          <dt className="text-[11px] uppercase tracking-widest text-text-mute">Owner</dt>
          <dd className="break-words">{owner ?? '—'}</dd>
        </div>
        <div>
          <dt className="text-[11px] uppercase tracking-widest text-text-mute">Read</dt>
          <dd className="break-words">{read.length > 0 ? read.join(', ') : '—'}</dd>
        </div>
        <div>
          <dt className="text-[11px] uppercase tracking-widest text-text-mute">Write</dt>
          <dd className="break-words">{write.length > 0 ? write.join(', ') : '—'}</dd>
        </div>
        <div>
          <dt className="text-[11px] uppercase tracking-widest text-text-mute">Your access</dt>
          <dd>{canWrite ? 'Read and write' : 'Read only'}</dd>
        </div>
        <div>
          <dt className="text-[11px] uppercase tracking-widest text-text-mute">Last maintenance</dt>
          <dd className="break-words">{maintenanceLine(catalog.maintenance)}</dd>
        </div>
      </dl>

      {!canWrite && owner && (
        <p className="text-xs text-text-mute">To write here, ask {owner} or an administrator to add your group.</p>
      )}

      <section className="space-y-2">
        <h2 className="text-sm font-semibold">Schemas ({schemas.length})</h2>
        {namespaces.isLoading ? (
          <p className="text-text-mute text-sm">loading…</p>
        ) : namespaces.isError ? (
          <ErrorBox
            error={namespaces.error}
            onRetry={forbidden(namespaces.error) ? undefined : () => void namespaces.refetch()}
          />
        ) : schemas.length === 0 ? (
          <div className="flex flex-wrap items-center gap-3">
            <p className="text-text-mute text-sm">This catalog has no schemas yet.</p>
            <Link to="/data" className="btn-ghost">Load data</Link>
          </div>
        ) : (
          <ul className="space-y-0.5">
            {schemas.map((parts) => {
              const ns = parts.join('.')
              return (
                <li key={ns}>
                  <button
                    type="button"
                    onClick={() => openSchema(ns)}
                    className="w-full flex items-center gap-2 text-left px-2 py-1.5 rounded-[8px] text-sm hover:bg-bg-soft/40 border border-transparent"
                  >
                    <ObjectIcon kind="schema" />
                    <span className="min-w-0 flex-1 truncate">{ns}</span>
                    <span aria-hidden="true" className="text-text-mute">→</span>
                  </button>
                </li>
              )
            })}
          </ul>
        )}
      </section>
    </div>
  )
}
