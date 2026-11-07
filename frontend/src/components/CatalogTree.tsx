import { useEffect, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useSearchParams } from 'react-router-dom'
import { ApiError, api, type Catalog } from '../api'
import ErrorBox from './ErrorBox'
import LayerBadge from './LayerBadge'
import ObjectIcon from './ObjectIcon'

export type CatalogTreeSelection = { ns: string; tbl: string }

/** A 403 says no: the person's grants do not change by asking again. */
function forbidden(error: unknown): boolean {
  return error instanceof ApiError && error.status === 403
}

export default function CatalogTree({
  catalogs,
  defaultCatalog,
  filter,
  selected,
  onSelect,
}: {
  catalogs: Catalog[]
  defaultCatalog: string
  filter: string
  selected: CatalogTreeSelection | null
  onSelect: (sel: CatalogTreeSelection) => void
}) {
  const [params, setParams] = useSearchParams()
  const urlCatalog = params.get('catalog')
  const deepLinkCatalog = urlCatalog ?? defaultCatalog

  return (
    <ul aria-label="Catalogs" className="space-y-0.5">
      {catalogs.map((catalog) => (
        <CatalogNode
          key={catalog.name}
          catalog={catalog}
          urlCatalog={urlCatalog}
          deepLinkCatalog={deepLinkCatalog}
          filter={filter}
          selected={selected}
          onSelect={onSelect}
          params={params}
          setParams={setParams}
        />
      ))}
    </ul>
  )
}

function CatalogNode({
  catalog,
  urlCatalog,
  deepLinkCatalog,
  filter,
  selected,
  onSelect,
  params,
  setParams,
}: {
  catalog: Catalog
  urlCatalog: string | null
  deepLinkCatalog: string
  filter: string
  selected: CatalogTreeSelection | null
  onSelect: (sel: CatalogTreeSelection) => void
  params: URLSearchParams
  setParams: (next: URLSearchParams | ((prev: URLSearchParams) => URLSearchParams), opts?: { replace?: boolean }) => void
}) {
  const initialOpen = catalog.name === deepLinkCatalog
  const [open, setOpen] = useState(initialOpen)

  useEffect(() => {
    if (catalog.name === deepLinkCatalog) setOpen(true)
  }, [catalog.name, deepLinkCatalog])

  const namespaces = useQuery({
    queryKey: ['catalog', 'namespaces', catalog.name],
    queryFn: () => api.namespaces(catalog.name),
    enabled: open,
  })

  const onCatalogClick = () => {
    if (catalog.name === urlCatalog) {
      // Already the URL catalog: toggle local collapse/expand.
      setOpen((o) => !o)
      return
    }
    // Select this catalog in the URL and open it.
    setOpen(true)
    const next = new URLSearchParams(params)
    next.set('catalog', catalog.name)
    next.delete('ns')
    next.delete('tbl')
    setParams(next)
  }

  return (
    <li>
      <button
        type="button"
        aria-expanded={open}
        onClick={onCatalogClick}
        className="w-full flex items-center gap-2 text-left px-2 py-1.5 rounded-[8px] hover:bg-bg-soft/40"
      >
        <span aria-hidden className="text-text-mute text-xs">
          {open ? '▾' : '▸'}
        </span>
        <ObjectIcon kind="catalog" />
        <span className="text-sm font-medium">{catalog.name}</span>
        <LayerBadge layer={catalog.layer} />
      </button>
      {open && (
        <ul className="ml-3 mt-0.5 space-y-0.5">
          {namespaces.isLoading && <li className="text-text-mute text-xs px-2 py-1">loading…</li>}
          {namespaces.isError && (
            <li className="px-2 py-1">
              <ErrorBox
                error={namespaces.error}
                onRetry={forbidden(namespaces.error) ? undefined : () => void namespaces.refetch()}
              />
            </li>
          )}
          {namespaces.data && namespaces.data.namespaces.length === 0 && (
            <li className="text-text-mute text-xs px-2 py-1">This catalog has no schemas yet.</li>
          )}
          <CatalogSchemas
            catalogName={catalog.name}
            namespaces={namespaces.data?.namespaces ?? []}
            deepLinkCatalog={deepLinkCatalog}
            filter={filter}
            selected={selected}
            onSelect={onSelect}
            params={params}
          />
        </ul>
      )}
    </li>
  )
}

function CatalogSchemas({
  catalogName,
  namespaces,
  deepLinkCatalog,
  filter,
  selected,
  onSelect,
  params,
}: {
  catalogName: string
  namespaces: string[][]
  deepLinkCatalog: string
  filter: string
  selected: CatalogTreeSelection | null
  onSelect: (sel: CatalogTreeSelection) => void
  params: URLSearchParams
}) {
  const filterTrimmed = filter.trim()
  // Track which schemas have visible matches (for "No matching tables" at catalog level)
  // But we need data from children... Use a render-prop pattern or compute from query cache.
  // Simpler: use useQueryClient to check cached table data for each schema.
  // But we don't have access to which schemas have been "evaluated". Easier approach:
  // Render all schemas; a schema returns null when filtered out and loaded with no matches.
  // Count visible schemas by... we can't count null returns from parent.
  // Alternative: have SchemaNode always render but hide itself? No, cleaner to return null.
  // Best practical approach: use a key trick or just compute match status here using query cache.
  return (
    <>
      {namespaces.map((parts) => {
        const ns = parts.join('.')
        return (
          <SchemaNode
            key={ns}
            ns={ns}
            catalogName={catalogName}
            deepLinkCatalog={deepLinkCatalog}
            filter={filter}
            selected={selected}
            onSelect={onSelect}
            params={params}
          />
        )
      })}
      {filterTrimmed && namespaces.length > 0 && (
        <NoMatchesMessage catalogName={catalogName} namespaces={namespaces} filter={filterTrimmed} />
      )}
    </>
  )
}

function NoMatchesMessage({
  catalogName,
  namespaces,
  filter,
}: {
  catalogName: string
  namespaces: string[][]
  filter: string
}) {
  // Check query cache for all schemas - if all that have data loaded have no matches, and none are still loading/fetching in a way that matters?
  // "a catalog with no matching loaded schema keeps its header and shows the sentence `No matching tables.`"
  // Use a simple approach: peek at window.__REACT_QUERY_STATE__? No. Better use useQueryClient.
  // But importing useQueryClient in a small component is fine.
  return <NoMatchesMessageInner catalogName={catalogName} namespaces={namespaces} filter={filter} />
}

function NoMatchesMessageInner({
  catalogName,
  namespaces,
  filter,
}: {
  catalogName: string
  namespaces: string[][]
  filter: string
}) {
  const qc = useQueryClient()
  const filterLower = filter.toLowerCase()
  let hasLoadedSchema = false
  let hasAnyMatch = false
  let hasUnloaded = false

  for (const parts of namespaces) {
    const ns = parts.join('.')
    const data = qc.getQueryData<{ identifiers: { namespace: string[]; name: string }[] }>([
      'catalog',
      'tables',
      catalogName,
      ns,
    ])
    if (!data) {
      hasUnloaded = true
      continue
    }
    hasLoadedSchema = true
    const match = data.identifiers.some((id) => `${ns}.${id.name}`.toLowerCase().includes(filterLower))
    if (match) hasAnyMatch = true
  }

  // Show message only when we have loaded schemas and none match, and there are no unloaded schemas
  // (unloaded schemas might match, so don't show "no matching tables")
  if (hasLoadedSchema && !hasAnyMatch && !hasUnloaded) {
    return <li className="text-text-mute text-xs px-2 py-1">No matching tables.</li>
  }
  return null
}

function SchemaNode({
  ns,
  catalogName,
  deepLinkCatalog,
  filter,
  selected,
  onSelect,
  params,
}: {
  ns: string
  catalogName: string
  deepLinkCatalog: string
  filter: string
  selected: CatalogTreeSelection | null
  onSelect: (sel: CatalogTreeSelection) => void
  params: URLSearchParams
}) {
  const urlNs = params.get('ns')
  const urlTbl = params.get('tbl')
  const initialOpen = catalogName === deepLinkCatalog && urlNs === ns && !!urlTbl
  const [open, setOpen] = useState(initialOpen)

  const tables = useQuery({
    queryKey: ['catalog', 'tables', catalogName, ns],
    queryFn: () => api.tables(ns, catalogName),
    enabled: open,
  })

  const filterTrimmed = filter.trim()
  const matches = (name: string) => {
    if (!filterTrimmed) return true
    return `${ns}.${name}`.toLowerCase().includes(filterTrimmed.toLowerCase())
  }
  const visible = (tables.data?.identifiers ?? []).filter((id) => matches(id.name))

  // "A schema with no table whose \"<schema>.<table>\" contains the filter is hidden"
  // Only hide when tables are loaded and none match. If not loaded (collapsed), don't hide.
  if (filterTrimmed && tables.data && visible.length === 0) return null

  return (
    <li>
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="w-full flex items-center gap-2 text-left px-2 py-1.5 rounded-[8px] hover:bg-bg-soft/40"
      >
        <span aria-hidden className="text-text-mute text-xs">
          {open ? '▾' : '▸'}
        </span>
        <ObjectIcon kind="schema" />
        <span className="text-sm font-medium">{ns}</span>
        {tables.data && <span className="ml-auto text-[11px] text-text-mute">{visible.length}</span>}
      </button>
      {open && (
        <ul className="ml-3 mt-0.5 space-y-0.5">
          {tables.isLoading && <li className="text-text-mute text-xs px-2 py-1">loading…</li>}
          {tables.isError && !tables.data && (
            <li className="px-2 py-1">
              <ErrorBox
                error={tables.error}
                onRetry={forbidden(tables.error) ? undefined : () => void tables.refetch()}
              />
            </li>
          )}
          {visible.map((id) => {
            const sel = selected?.ns === ns && selected?.tbl === id.name
            return (
              <li key={id.name}>
                <button
                  type="button"
                  onClick={() => onSelect({ ns, tbl: id.name })}
                  aria-current={sel ? 'true' : undefined}
                  className={[
                    'w-full text-left px-2 py-1.5 rounded-[8px] text-sm flex items-center gap-2',
                    sel
                      ? 'bg-accent/15 text-text border border-accent/30'
                      : 'text-text-dim hover:text-text hover:bg-bg-soft/40 border border-transparent',
                  ].join(' ')}
                >
                  <ObjectIcon kind={id.kind === 'view' ? 'view' : 'table'} />
                  <span className="truncate">{id.name}</span>
                </button>
              </li>
            )
          })}
        </ul>
      )}
    </li>
  )
}
