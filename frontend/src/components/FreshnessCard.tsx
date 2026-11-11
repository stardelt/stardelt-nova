import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api } from '../api'
import { formatCount, relativeTime } from '../format'
import ErrorBox from './ErrorBox'
import { EmptyState } from './EmptyState'
import { Skeleton } from './Skeleton'

/** 7 days: the window is 30, so a table nothing wrote for a week is worth an amber line. */
const STALE_MS = 7 * 86_400_000

/** A namespace is `<catalog>.<schema>`; demo data may carry the schema alone. */
function splitNamespace(namespace: string): { catalog: string; schema: string } {
  const dot = namespace.indexOf('.')
  return dot > 0
    ? { catalog: namespace.slice(0, dot), schema: namespace.slice(dot + 1) }
    : { catalog: '', schema: namespace }
}

/**
 * The tables this person reads most, and when each last changed (#510 slice 4b). The API
 * caps the list at five; a table nothing wrote for a week, or at all in the window, is
 * called out rather than hidden, because that is the answer the card exists for.
 */
export default function FreshnessCard() {
  const tables = useQuery({
    queryKey: ['read-tables'],
    queryFn: () => api.readTables(),
    staleTime: 60_000,
    retry: false,
  })
  const rows = tables.data?.tables ?? []
  const now = Date.now()
  return (
    <section className="card p-5 min-w-0" aria-labelledby="freshness-title">
      <div className="flex items-baseline justify-between gap-2">
        <h2 id="freshness-title" className="text-sm font-semibold">Freshness</h2>
        <span className="text-xs text-text-mute">The five tables you read most · 30 days</span>
      </div>
      {tables.isError && (
        <div className="mt-3">
          <ErrorBox error={tables.error} onRetry={() => void tables.refetch()} />
        </div>
      )}
      {!tables.isError && tables.data === undefined && (
        <div className="mt-3 space-y-2">
          {[0, 1, 2].map((i) => <Skeleton key={i} className="h-6" />)}
        </div>
      )}
      {!tables.isError && tables.data !== undefined && rows.length === 0 && (
        <EmptyState icon="clock" title="No tables read yet">Run a query and the tables you use show up here.</EmptyState>
      )}
      {rows.length > 0 && (
        <ul className="mt-3 divide-y divide-border">
          {rows.map((t) => {
            const { catalog, schema } = splitNamespace(t.namespace)
            const qs = new URLSearchParams()
            if (catalog !== '') qs.set('catalog', catalog)
            qs.set('ns', schema)
            qs.set('tbl', t.name)
            const wrote = Date.parse(t.lastWrite ?? '')
            const stale = Number.isFinite(wrote) && now - wrote > STALE_MS
            return (
              <li key={`${t.namespace}.${t.name}`}>
                <Link to={`/catalog?${qs.toString()}`} className="flex items-center justify-between gap-3 py-2 hover:text-text">
                  <span className="font-mono text-sm truncate">{t.namespace}.{t.name}</span>
                  <span className="text-xs shrink-0">
                    {formatCount(t.queries)} {t.queries === 1 ? 'query' : 'queries'}
                    {' · '}
                    {Number.isFinite(wrote) ? (
                      <span className={stale ? 'text-warn' : 'text-text-mute'}>
                        changed {relativeTime(wrote, now)}
                      </span>
                    ) : (
                      <span className="text-warn">No write in 30 days</span>
                    )}
                  </span>
                </Link>
              </li>
            )
          })}
        </ul>
      )}
    </section>
  )
}
