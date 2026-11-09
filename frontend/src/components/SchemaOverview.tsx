import { useQuery } from '@tanstack/react-query'
import { Link, useSearchParams } from 'react-router-dom'
import { ApiError, api } from '../api'
import ErrorBox from './ErrorBox'
import ObjectIcon from './ObjectIcon'

/** A 403 says no: the person's grants do not change by asking again. */
function forbidden(error: unknown): boolean {
  return error instanceof ApiError && error.status === 403
}

/**
 * The right pane's page for a schema (stardelt-nova#585): its tables and views, between the
 * catalog page and the table page. A row opens the table page, `?catalog=C&ns=S&tbl=T`; the
 * list is the tree's own query, so a schema the tree has loaded shows at once.
 */
export default function SchemaOverview({ catalog, ns }: { catalog: string; ns: string }) {
  const [, setParams] = useSearchParams()
  const tables = useQuery({
    queryKey: ['catalog', 'tables', catalog, ns],
    queryFn: () => api.tables(ns, catalog),
  })
  const identifiers = tables.data?.identifiers ?? []

  const openTable = (tbl: string) => {
    setParams(new URLSearchParams({ catalog, ns, tbl }))
  }

  return (
    <div className="card p-5 space-y-4">
      <h1 className="text-xl font-semibold flex items-center gap-2 flex-wrap">
        <ObjectIcon kind="schema" />
        <span className="font-mono break-words">{ns}</span>
        <span className="text-text-mute text-sm font-normal">in {catalog}</span>
      </h1>

      <div className="flex flex-wrap items-center justify-between gap-3">
        <h2 className="text-sm font-semibold">Tables and views ({identifiers.length})</h2>
        <Link to="/data" className="btn-ghost">Load data</Link>
      </div>

      {tables.isLoading ? (
        <p className="text-text-mute text-sm">loading…</p>
      ) : tables.isError ? (
        <ErrorBox
          error={tables.error}
          onRetry={forbidden(tables.error) ? undefined : () => void tables.refetch()}
        />
      ) : identifiers.length === 0 ? (
        <p className="text-text-mute text-sm">This schema has no tables yet.</p>
      ) : (
        <ul className="space-y-0.5">
          {identifiers.map((id) => (
            <li key={id.name}>
              <button
                type="button"
                onClick={() => openTable(id.name)}
                className="w-full flex items-center gap-2 text-left px-2 py-1.5 rounded-[8px] text-sm hover:bg-bg-soft/40 border border-transparent"
              >
                <ObjectIcon kind={id.kind === 'view' ? 'view' : 'table'} />
                <span className="min-w-0 flex-1 truncate">{id.name}</span>
                <span className="text-text-mute text-xs">{id.kind === 'view' ? 'view' : 'table'}</span>
                <span aria-hidden="true" className="text-text-mute">→</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
