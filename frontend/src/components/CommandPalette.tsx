import { Fragment, useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from 'react'
import { useNavigate } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { Icon, type IconName } from './Icon'
import { api } from '../api'
import { NAV_GROUPS } from '../nav'

/** One row of the palette: a page, or a table the catalog search found. */
type Row = { key: string; group: string; label: string; hint: string; icon: IconName; to: string }

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
 * open the SQL editor with that SQL. Actions arrive in a later slice.
 */
export default function CommandPalette({
  open,
  onClose,
  admin,
}: {
  open: boolean
  onClose: () => void
  admin: boolean
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

  const rows = useMemo(() => [...pages, ...savedRows, ...tableRows], [pages, savedRows, tableRows])

  if (!open) return null

  function go(to: string) {
    navigate(to)
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
      go(row.to)
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
            aria-label="Search pages, queries and tables"
            aria-expanded="true"
            aria-controls={`${id}-list`}
            aria-activedescendant={activeId}
            autoFocus
            value={query}
            placeholder="Search pages, queries and tables…"
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
                  onClick={() => go(row.to)}
                  onMouseEnter={() => setActive(i)}
                  className={[
                    'flex cursor-pointer items-center gap-2.5 rounded-[10px] px-3 py-2 text-sm',
                    i === active ? 'bg-accent/15 text-text' : 'text-text-dim hover:bg-bg-soft',
                  ].join(' ')}
                >
                  <Icon name={row.icon} className="w-4 h-4 shrink-0" />
                  <span className="truncate">{row.label}</span>
                  <span className="ml-auto font-mono text-[11px] text-text-mute">{row.hint}</span>
                </li>
              </Fragment>
            ))}
          </ul>
        )}
      </div>
    </dialog>
  )
}
