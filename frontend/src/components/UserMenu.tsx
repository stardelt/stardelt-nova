import { useEffect, useRef, useState } from 'react'
import { Link } from 'react-router-dom'
import { isAdmin } from '../roles'
import type { Me } from '../api'

function initial(name?: string | null): string {
  const s = (name ?? '').trim()
  if (!s) return '?'
  return s.slice(0, 1).toUpperCase()
}

export default function UserMenu({ me }: { me?: Me }) {
  const [open, setOpen] = useState(false)
  const wrapper = useRef<HTMLDivElement>(null)
  const button = useRef<HTMLButtonElement>(null)
  const menuRef = useRef<HTMLDivElement>(null)
  const itemRefs = useRef<(HTMLAnchorElement | HTMLButtonElement)[]>([])

  const admin = isAdmin(me)
  const name = me?.user ?? (me ? 'unknown user' : 'loading…')
  const email = me?.email ?? null
  const authMode = me?.auth_mode
  const dev = authMode === 'static-dev'

  useEffect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault()
        setOpen(false)
        button.current?.focus()
        return
      }
      if (e.key === 'Tab') {
        setOpen(false)
        return
      }
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault()
        const items = itemRefs.current.filter(Boolean)
        if (items.length === 0) return
        const idx = items.indexOf(document.activeElement as HTMLAnchorElement | HTMLButtonElement)
        let next = idx
        if (e.key === 'ArrowDown') {
          next = idx < 0 ? 0 : (idx + 1) % items.length
        } else {
          next = idx < 0 ? items.length - 1 : (idx - 1 + items.length) % items.length
        }
        items[next]?.focus()
        return
      }
      if (e.key === 'Home') {
        e.preventDefault()
        const items = itemRefs.current.filter(Boolean)
        if (items.length > 0) items[0]?.focus()
        return
      }
      if (e.key === 'End') {
        e.preventDefault()
        const items = itemRefs.current.filter(Boolean)
        if (items.length > 0) items[items.length - 1]?.focus()
        return
      }
    }
    const onClick = (e: MouseEvent) => {
      if (wrapper.current && !wrapper.current.contains(e.target as Node)) {
        setOpen(false)
      }
    }
    document.addEventListener('keydown', onKey)
    document.addEventListener('mousedown', onClick)
    return () => {
      document.removeEventListener('keydown', onKey)
      document.removeEventListener('mousedown', onClick)
    }
  }, [open])

  useEffect(() => {
    if (open) {
      // Focus first item when opened
      const items = itemRefs.current.filter(Boolean)
      if (items.length > 0) {
        // Small delay to let panel render
        requestAnimationFrame(() => items[0]?.focus())
      }
    }
  }, [open])

  function toggle() {
    if (open) {
      setOpen(false)
    } else {
      setOpen(true)
    }
  }

  function close() {
    setOpen(false)
  }

  const items: Array<HTMLAnchorElement | HTMLButtonElement> = []

  return (
    <div ref={wrapper} className="relative">
      <button
        ref={button}
        type="button"
        className="btn-ghost py-1.5 text-sm flex items-center gap-2"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={`Account menu for ${name}`}
        onClick={toggle}
      >
        <div className="w-7 h-7 rounded-full bg-accent/20 border border-accent/30 grid place-items-center text-xs font-semibold text-accent">
          {initial(name)}
        </div>
        <span className="hidden sm:inline truncate max-w-[12rem]" title={name}>
          {name}
        </span>
      </button>
      {open && (
        <div
          ref={menuRef}
          role="menu"
          aria-label="Account menu"
          className="absolute right-0 top-full mt-1 z-50 min-w-[16rem] max-w-[calc(100vw-1rem)] rounded-[12px] border border-border bg-bg-elev shadow-2xl p-1.5"
        >
          <div className="px-3 py-2 border-b border-border mb-1">
            <div className="text-sm font-medium break-words">{name}</div>
            {email && <div className="text-[11px] text-text-mute break-words">{email}</div>}
            {admin && <div className="text-[11px] text-accent mt-1">Administrator</div>}
            {dev && (
              <div className="text-[11px] text-text-mute mt-1">Development mode (no sign-in)</div>
            )}
          </div>
          <Link
            to="/profile"
            role="menuitem"
            className="flex items-center px-3 py-2 rounded-[8px] text-sm hover:bg-bg-soft focus:bg-bg-soft outline-none"
            onClick={close}
            ref={(el) => {
              if (el) items.push(el)
              itemRefs.current = items.slice()
            }}
          >
            Profile
          </Link>
          <Link
            to="/preferences"
            role="menuitem"
            className="flex items-center px-3 py-2 rounded-[8px] text-sm hover:bg-bg-soft focus:bg-bg-soft outline-none"
            onClick={close}
            ref={(el) => {
              if (el) items.push(el)
              itemRefs.current = items.slice()
            }}
          >
            Preferences
          </Link>
          {admin && (
            <Link
              to="/settings"
              role="menuitem"
              className="flex items-center px-3 py-2 rounded-[8px] text-sm hover:bg-bg-soft focus:bg-bg-soft outline-none"
              onClick={close}
              ref={(el) => {
                if (el) items.push(el)
                itemRefs.current = items.slice()
              }}
            >
              Platform settings
            </Link>
          )}
          {authMode === 'oidc' && (
            <>
              <div className="my-1 border-t border-border" />
              <form method="post" action="/api/auth/logout" className="w-full">
                <button
                  type="submit"
                  role="menuitem"
                  className="w-full text-left flex items-center px-3 py-2 rounded-[8px] text-sm hover:bg-bg-soft focus:bg-bg-soft outline-none"
                  title="Sign out of Nova and all connected apps"
                  aria-label="Sign out of Nova and all connected apps"
                  onClick={() => {
                    // Let form submit normally
                    close()
                  }}
                  ref={(el) => {
                    if (el) items.push(el)
                    itemRefs.current = items.slice()
                  }}
                >
                  Sign out
                </button>
              </form>
            </>
          )}
        </div>
      )}
    </div>
  )
}
