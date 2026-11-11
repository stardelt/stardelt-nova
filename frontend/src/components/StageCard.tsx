import { useEffect, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import type { Me } from '../api'
import { useYourWarehouse, warehouseHint, warehouseText } from './WarehouseNote'

/**
 * The top-left card answers "where am I working?": the stage this Nova is (one
 * `PlatformInstance`), the SQL class this person's queries run on, and a way to the sibling
 * stages. The default *catalog* that used to sit here (`me.warehouse`) stays on the SQL editor's
 * picker and on Platform settings — it is not compute and it cannot be chosen.
 */

/** The host of a peer's URL, for the muted second column; the raw value when it is not a URL. */
function hostOf(url: string): string {
  try {
    return new URL(url).host
  } catch {
    return url
  }
}

export default function StageCard({ me, collapsed }: { me?: Me | null; collapsed: boolean }) {
  const warehouse = useYourWarehouse()
  const [open, setOpen] = useState(false)
  const wrapper = useRef<HTMLDivElement>(null)
  const button = useRef<HTMLButtonElement>(null)

  const name = me?.stage_name ?? window.location.host
  const kind = me?.stage_kind ?? null
  const peers = me?.peers ?? []
  const switchable = peers.length > 0

  const dotColour =
    kind === 'prod' ? 'bg-amber-500' : kind === 'test' ? 'bg-accent' : 'bg-text-mute'

  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault()
        setOpen(false)
        button.current?.focus()
      }
    }
    const onClick = (e: MouseEvent) => {
      if (wrapper.current && !wrapper.current.contains(e.target as Node)) setOpen(false)
    }
    document.addEventListener('keydown', onKey)
    document.addEventListener('mousedown', onClick)
    return () => {
      document.removeEventListener('keydown', onKey)
      document.removeEventListener('mousedown', onClick)
    }
  }, [open])

  // A peer opens another deployment: same page, other host.
  const here = window.location.pathname + window.location.search

  const body = (
    <>
      {collapsed ? (
        // The rail shows the dot alone; the `demo` badge shrinks to a dot on it.
        <span className="relative inline-flex shrink-0 h-4 w-4 items-center justify-center">
          <span className={`h-2 w-2 rounded-full ${dotColour}`} />
          {me?.demo && (
            <span
              title="demo"
              className="hidden md:block absolute -top-0.5 -right-0.5 h-1.5 w-1.5 rounded-full bg-warn"
            />
          )}
        </span>
      ) : (
        <span className={`h-2 w-2 rounded-full shrink-0 ${dotColour}`} />
      )}
      <div className={collapsed ? 'min-w-0 md:hidden' : 'min-w-0'}>
        <div className="text-[10px] uppercase tracking-widest text-text-mute">
          {me?.stage_name ? 'Stage' : 'Platform'}
        </div>
        <div className="text-sm font-medium truncate" title={name}>{name}</div>
        <div
          className="text-xs text-text-mute truncate"
          title={warehouse ? warehouseHint(warehouse) : undefined}
        >
          {warehouse ? `SQL: ${warehouseText(warehouse)}` : 'SQL: —'}
        </div>
      </div>
      {me?.demo && (
        <span
          className={[
            'badge border-amber-400/40 bg-amber-400/10 text-warn ml-auto',
            collapsed ? 'md:hidden' : '',
          ].filter(Boolean).join(' ')}
        >
          demo
        </span>
      )}
    </>
  )

  const cardClass = [
    'card w-full text-left flex items-center gap-2',
    switchable ? 'hover:bg-bg-soft' : '',
    collapsed ? 'md:justify-center md:px-2 md:py-2 px-3 py-2' : 'px-3 py-2',
  ].filter(Boolean).join(' ')

  return (
    <div ref={wrapper} className="relative">
      {switchable ? (
        <button
          ref={button}
          type="button"
          className={cardClass}
          title={collapsed ? name : 'Switch stage'}
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={() => setOpen(v => !v)}
        >
          {body}
        </button>
      ) : (
        <div className={cardClass} title={collapsed ? name : undefined}>{body}</div>
      )}
      {open && switchable && (
        <div
          role="menu"
          aria-label="Switch stage"
          className="absolute left-0 top-full mt-1 z-50 min-w-[15rem] rounded-[12px] border border-border bg-bg-elev shadow-2xl p-1.5"
        >
          {peers.map(peer => (
            <a
              key={peer.url}
              role="menuitem"
              href={peer.url.replace(/\/$/, '') + here}
              className="flex items-center justify-between gap-3 px-3 py-2 rounded-[8px] text-sm hover:bg-bg-soft focus:bg-bg-soft outline-none"
            >
              <span className="truncate">{peer.name}</span>
              <span className="text-[11px] text-text-mute truncate">{hostOf(peer.url)}</span>
            </a>
          ))}
          <div className="my-1 border-t border-border" />
          <Link
            to="/deployments"
            role="menuitem"
            className="flex items-center px-3 py-2 rounded-[8px] text-sm hover:bg-bg-soft focus:bg-bg-soft outline-none"
            onClick={() => setOpen(false)}
          >
            Compare stages
          </Link>
        </div>
      )}
    </div>
  )
}
