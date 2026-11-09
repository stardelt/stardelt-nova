import { Fragment, useEffect, useId, useMemo, useRef, useState, type KeyboardEvent } from 'react'
import { useNavigate } from 'react-router-dom'
import { Icon } from './Icon'
import { NAV_GROUPS, type NavItem } from '../nav'

/**
 * ⌘K / Ctrl+K: a keyboard-first way to reach a page.
 *
 * The list is `NAV_GROUPS`, filtered by the same admin rule as the sidebar, so the
 * palette can never offer a page the person cannot see in navigation. A native
 * `<dialog>` opened with `showModal()` gives focus trapping, an inert page behind it
 * and Escape for free; the query starts empty on every open.
 *
 * This slice is pages only: tables, queries and actions arrive in later slices.
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

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase()
    const out: { group: string; item: NavItem }[] = []
    for (const group of NAV_GROUPS) {
      if (group.admin && !admin) continue
      let shown = 0
      for (const item of group.items) {
        if (q && !item.label.toLowerCase().includes(q) && !item.to.toLowerCase().includes(q)) continue
        if (shown >= 8) break
        out.push({ group: group.label, item })
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
    return () => {
      if (dialog.open) dialog.close()
      opener.current?.focus()
    }
  }, [open])

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
      go(row.item.to)
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
            aria-label="Search pages"
            aria-expanded="true"
            aria-controls={`${id}-list`}
            aria-activedescendant={activeId}
            autoFocus
            value={query}
            placeholder="Search pages…"
            className="input w-full"
            onChange={(e) => {
              setQuery(e.target.value)
              setActive(0)
            }}
            onKeyDown={onKeyDown}
          />
        </div>
        {rows.length === 0 ? (
          <p className="px-4 py-6 text-text-mute text-sm">No pages match “{query}”.</p>
        ) : (
          <ul id={`${id}-list`} role="listbox" aria-label="Pages" className="min-h-0 overflow-y-auto p-1.5">
            {rows.map((row, i) => (
              <Fragment key={row.item.to}>
                {rows[i - 1]?.group !== row.group && (
                  <li role="presentation" className="label px-3 pb-1 pt-2">{row.group}</li>
                )}
                <li
                  role="option"
                  id={`${id}-opt-${i}`}
                  aria-selected={i === active}
                  onClick={() => go(row.item.to)}
                  onMouseEnter={() => setActive(i)}
                  className={[
                    'flex cursor-pointer items-center gap-2.5 rounded-[10px] px-3 py-2 text-sm',
                    i === active ? 'bg-accent/15 text-text' : 'text-text-dim hover:bg-bg-soft',
                  ].join(' ')}
                >
                  <Icon name={row.item.icon} className="w-4 h-4 shrink-0" />
                  <span className="truncate">{row.item.label}</span>
                  <span className="ml-auto font-mono text-[11px] text-text-mute">{row.item.to}</span>
                </li>
              </Fragment>
            ))}
          </ul>
        )}
      </div>
    </dialog>
  )
}
