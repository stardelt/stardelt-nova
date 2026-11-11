import { Fragment, useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from 'react'
import { useNavigate } from 'react-router-dom'
import { useQueries, useQuery } from '@tanstack/react-query'
import { Icon, type IconName } from './Icon'
import { api } from '../api'
import { formatTime } from '../format'
import { NAV_GROUPS } from '../nav'

/** One row of the palette: a page, a table the catalog search found, or an action. */
type Row = { key: string; group: string; label: string; hint: string; icon: IconName; to: string; run?: () => void }

/**
 * A statement short enough for one palette row: whitespace collapsed to single spaces,
 * cut at `n` characters with an ellipsis when it is longer.
 */
function firstWords(sql: string, n: number): string {
  const flat = sql.replace(/\s+/g, ' ').trim()
  return flat.length > n ? `${flat.slice(0, n)}…` : flat
}

/**
 * ⌘K / Ctrl+K: a keyboard-first way to reach a page or a table.
 *
 * The page list is `NAV_GROUPS`, filtered by the same admin rule as the sidebar, so the
 * palette can never offer a page the person cannot see in navigation. Tables come from
 * `GET /api/catalog/search` once the query is two characters long; a result opens the
 * catalog explorer with that table selected. A native `<dialog>` opened with `showModal()`
 * gives focus trapping, an inert page behind it and Escape for free; the query starts
 * empty on every open.
 *
 * Saved queries come from `GET /api/saved-queries`, matched on their title or their SQL, and
 * open the SQL editor with that SQL. Pipelines come from `GET /api/pipelines` and open the
 * pipeline's page; the API already hides a pipeline the person may not see. Recent queries
 * come from `GET /api/query-log?scope=mine`
 * and open the query log with that query expanded. The last group is the five actions a person
 * does from the palette: four of them navigate, and "Switch theme" runs the header's own toggle.
 * Catalogs and schemas come from the same lists as the Catalog explorer; a schema opens its schema page.
 */
export default function CommandPalette({
  open,
  onClose,
  admin,
  onSwitchTheme,
}: {
  open: boolean
  onClose: () => void
  admin: boolean
  onSwitchTheme: () => void
}) {
  const ref = useRef<HTMLDialogElement>(null)
  const opener = useRef<HTMLElement | null>(null)
  const navigate = useNavigate()
  const id = useId()
  const [query, setQuery] = useState('')
  const [active, setActive] = useState(0)

  const pages = useMemo<Row[]>(() => {
    const q = query.trim().toLowerCase()
    const out: Row[] = []
    for (const group of NAV_GROUPS) {
      if (group.admin && !admin) continue
      let shown = 0
      for (const item of group.items) {
        if (q && !item.label.toLowerCase().includes(q) && !item.to.toLowerCase().includes(q)) continue
        if (shown >= 8) break
        out.push({ key: item.to, group: group.label, label: item.label, hint: item.to, icon: item.icon, to: item.to })
        shown++
      }
    }
    return out
  }, [admin, query])

  useEffect(() => {
    if (!open) return
    const dialog = ref.current
    if (!dialog) return
    opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null
    if (!dialog.open) dialog.showModal()
    setQuery('')
    setActive(0)
    setTableQuery('')
    return () => {
      if (dialog.open) dialog.close()
      opener.current?.focus()
    }
  }, [open])

  const [tableQuery, setTableQuery] = useState('')
  useEffect(() => {
    if (!open) return
    const timer = window.setTimeout(() => setTableQuery(query.trim()), 200)
    return () => window.clearTimeout(timer)
  }, [open, query])

  const tables = useQuery({
    queryKey: ['palette-tables', tableQuery],
    queryFn: () => api.searchTables(tableQuery, 8),
    enabled: open && tableQuery.length >= 2,
    staleTime: 30_000,
    retry: false,
  })

  const tableRows = useMemo<Row[]>(
    () =>
      (tables.data?.matches ?? []).slice(0, 8).map((m) => ({
        key: `table:${m.catalog}:${m.namespace}:${m.table}`,
        group: 'Tables',
        label: `${m.namespace}.${m.table}`,
        hint: m.catalog,
        icon: 'table',
        to: `/catalog?catalog=${encodeURIComponent(m.catalog)}&ns=${encodeURIComponent(m.namespace)}&tbl=${encodeURIComponent(m.table)}`,
      })),
    [tables.data],
  )

  // Catalogs and their schemas (nova: palette catalogs + schemas). The API already hides a
  // catalog the person may not browse. Fetched once per dialog opening; at most 20 catalogs.
  const catalogs = useQuery({
    queryKey: ['palette-catalogs'],
    queryFn: () => api.catalogs(),
    enabled: open,
    staleTime: 60_000,
    retry: false,
  })
  const catalogNames = (catalogs.data?.catalogs ?? []).map(c => c.name).slice(0, 20)
  const schemaLists = useQueries({
    queries: catalogNames.map(name => ({
      queryKey: ['palette-namespaces', name],
      queryFn: () => api.namespaces(name),
      enabled: open && query.trim().length >= 1,
      staleTime: 60_000,
      retry: false,
    })),
  })

  const catalogRows = useMemo<Row[]>(() => {
    const q = query.trim().toLowerCase()
    if (!q) return []
    const fallback = catalogs.data?.default
    return catalogNames
      .filter(name => name.toLowerCase().includes(q))
      .slice(0, 5)
      .map(name => ({
        key: `catalog:${name}`,
        group: 'Catalogs',
        label: name,
        hint: name === fallback ? 'Default catalog' : 'Catalog',
        icon: 'database' as const,
        to: `/catalog?catalog=${encodeURIComponent(name)}`,
      }))
  }, [query, catalogs.data])

  const schemaRows = useMemo<Row[]>(() => {
    const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean)
    if (words.length === 0) return []
    const out: Row[] = []
    catalogNames.forEach((name, i) => {
      for (const parts of schemaLists[i]?.data?.namespaces ?? []) {
        const ns = parts.join('.')
        const full = `${name}.${ns}`.toLowerCase()
        if (!words.every(w => full.includes(w))) continue
        out.push({
          key: `schema:${name}:${ns}`,
          group: 'Schemas',
          label: `${name}.${ns}`,
          hint: 'Schema',
          icon: 'folder',
          to: `/catalog?catalog=${encodeURIComponent(name)}&ns=${encodeURIComponent(ns)}`,
        })
      }
    })
    return out.slice(0, 8)
  }, [query, catalogs.data, schemaLists])

  const saved = useQuery({
    queryKey: ['saved-queries'],
    queryFn: () => api.savedQueries(),
    enabled: open,
    staleTime: 30_000,
    retry: false,
  })

  const savedRows = useMemo<Row[]>(() => {
    const q = query.trim().toLowerCase()
    if (!q) return []
    return (saved.data ?? [])
      .filter(s => s.title.toLowerCase().includes(q) || s.sql.toLowerCase().includes(q))
      .slice(0, 8)
      .map(s => ({
        key: `saved:${s.id}`,
        group: 'Saved queries',
        label: s.title,
        hint: 'Query',
        icon: 'terminal' as const,
        to: `/query?sql=${encodeURIComponent(s.sql)}`,
      }))
  }, [query, saved.data])

  // The queries this person ran, newest first. Fetched only once the query is two
  // characters long, so a single keystroke does not hit the engine's log.
  const recent = useQuery({
    queryKey: ['palette-recent-queries'],
    queryFn: () => api.queryLog({ scope: 'mine', limit: 20 }),
    enabled: open && query.trim().length >= 2,
    staleTime: 30_000,
    retry: false,
  })

  const queryRows = useMemo<Row[]>(() => {
    const q = query.trim().toLowerCase()
    if (!q) return []
    return (recent.data?.items ?? [])
      .filter(item => item.sql.toLowerCase().includes(q) || (item.queryType ?? '').toLowerCase().includes(q))
      .slice(0, 8)
      .map(item => ({
        key: `query:${item.id}`,
        group: 'Queries',
        label: firstWords(item.sql, 60),
        hint: formatTime(item.createdAt),
        icon: 'clock' as const,
        to: `/queries?id=${encodeURIComponent(item.id)}`,
      }))
  }, [query, recent.data])

  // The person's pipelines. The API already hides a pipeline they may not see, so this adds
  // no visibility rule of its own. The list is short; it is fetched once per dialog opening.
  const pipelines = useQuery({
    queryKey: ['palette-pipelines'],
    queryFn: () => api.pipelines(),
    enabled: open,
    staleTime: 30_000,
    retry: false,
  })

  const pipelineRows = useMemo<Row[]>(() => {
    const q = query.trim().toLowerCase()
    if (!q) return []
    return (pipelines.data?.pipelines ?? [])
      .filter(p => `${p.namespace}/${p.name}`.toLowerCase().includes(q))
      .slice(0, 8)
      .map(p => ({
        key: `pipeline:${p.namespace}:${p.name}`,
        group: 'Pipelines',
        label: `${p.namespace}/${p.name}`,
        hint: p.paused ? 'Paused' : (p.schedule ?? 'On demand'),
        icon: 'play' as const,
        to: `/pipelines/${encodeURIComponent(p.namespace)}/${encodeURIComponent(p.name)}`,
      }))
  }, [query, pipelines.data])

  const actions = useMemo<Row[]>(() => {
    const q = query.trim().toLowerCase()
    const all: Row[] = [
      { key: 'action:new-query',   group: 'Actions', label: 'New query',   hint: 'Open the SQL editor', icon: 'terminal', to: '/query' },
      { key: 'action:load-data',   group: 'Actions', label: 'Load data',   hint: 'Files, uploads and loads', icon: 'upload', to: '/data' },
      { key: 'action:query-log',   group: 'Actions', label: 'Query log',   hint: 'Recent queries', icon: 'clock', to: '/queries' },
      { key: 'action:preferences', group: 'Actions', label: 'Preferences', hint: 'Theme and navigation', icon: 'sliders', to: '/preferences' },
      { key: 'action:theme',       group: 'Actions', label: 'Switch theme', hint: 'The paired theme', icon: 'sun', to: '', run: onSwitchTheme },
    ]
    return all.filter(a => !q || a.label.toLowerCase().includes(q) || a.hint.toLowerCase().includes(q))
  }, [query, onSwitchTheme])

  const rows = useMemo(
    () => [...pages, ...catalogRows, ...schemaRows, ...tableRows, ...pipelineRows, ...queryRows, ...savedRows, ...actions],
    [pages, catalogRows, schemaRows, tableRows, pipelineRows, queryRows, savedRows, actions],
  )

  if (!open) return null

  function go(row: Row) {
    if (row.run) row.run()
    else navigate(row.to)
    onClose()
  }

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === 'ArrowDown') {
      event.preventDefault()
      setActive(index => Math.min(index + 1, Math.max(rows.length - 1, 0)))
    } else if (event.key === 'ArrowUp') {
      event.preventDefault()
      setActive(index => Math.max(index - 1, 0))
    } else if (event.key === 'Enter') {
      const row = rows[active]
      if (!row) return
      event.preventDefault()
      go(row)
    }
  }

  const activeId = rows[active] ? `${id}-opt-${active}` : undefined

  return (
    <dialog
      ref={ref}
      aria-label="Command palette"
      onCancel={(event) => {
        event.preventDefault()
        onClose()
      }}
      className="mt-[20vh] mx-auto w-[min(40rem,calc(100vw_-_2rem))] max-w-none rounded-[14px] border border-border bg-bg-elev p-0 text-text shadow-2xl backdrop:bg-black/60"
    >
      <div className="flex max-h-[70vh] flex-col">
        <div className="border-b border-border p-3">
          <input
            role="combobox"
            aria-label="Search pages, catalogs, schemas, tables and queries"
            aria-expanded="true"
            aria-controls={`${id}-list`}
            aria-activedescendant={activeId}
            autoFocus
            value={query}
            placeholder="Search catalogs, schemas, tables, pages…"
            className="input w-full"
            onChange={(e) => {
              setQuery(e.target.value)
              setActive(0)
            }}
            onKeyDown={onKeyDown}
          />
        </div>
        {rows.length === 0 ? (
          <p className="px-4 py-6 text-text-mute text-sm">Nothing matches “{query}”.</p>
        ) : (
          <ul id={`${id}-list`} role="listbox" aria-label="Results" className="min-h-0 overflow-y-auto p-1.5">
            {rows.map((row, i) => (
              <Fragment key={row.key}>
                {rows[i - 1]?.group !== row.group && (
                  <li role="presentation" className="label px-3 pb-1 pt-2">{row.group}</li>
                )}
                <li
                  role="option"
                  id={`${id}-opt-${i}`}
                  aria-selected={i === active}
                  onClick={() => go(row)}
                  onMouseEnter={() => setActive(i)}
                  className={[
                    'flex cursor-pointer items-center gap-2.5 rounded-[10px] px-3 py-2 text-sm',
                    i === active ? 'bg-accent/15 text-text' : 'text-text-dim hover:bg-bg-soft',
                  ].join(' ')}
                >
                  <Icon name={row.icon} className="w-4 h-4 shrink-0" />
                  <span className="truncate">{row.label}</span>
                  <span className="ml-auto text-xs text-text-mute">{row.hint}</span>
                </li>
              </Fragment>
            ))}
          </ul>
        )}
      </div>
    </dialog>
  )
}
