import { useSyncExternalStore, type ReactNode } from 'react'

export type CollapsiblePaneId = 'tables' | 'history'

const STORAGE_PREFIX = 'stardelt-nova:sql-pane:'

/** The choices this page made; `undefined` means "not chosen in this page, read storage". */
const chosen: Partial<Record<CollapsiblePaneId, boolean>> = {}
const listeners = new Set<() => void>()

/**
 * Whether this viewport shows the two-pane layout, Tailwind's `lg` (64rem). The SQL editor
 * gives its width to the editor on a phone, so both panes start collapsed there.
 */
function wideViewport(): boolean {
  try {
    if (typeof window === 'undefined') return true
    return window.matchMedia('(min-width: 64rem)').matches
  } catch {
    return true
  }
}

/** The person's store choice, or null where there is none (private windows, blocked storage). */
function stored(id: CollapsiblePaneId): boolean | null {
  try {
    const value = window.localStorage.getItem(STORAGE_PREFIX + id)
    return value === null ? null : value !== '0'
  } catch {
    return null
  }
}

function isOpen(id: CollapsiblePaneId): boolean {
  return chosen[id] ?? stored(id) ?? wideViewport()
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
  }
}

/**
 * Whether the named pane is open. The SQL editor reads both states to build its
 * `grid-template-columns`, so the state lives beside the component rather than inside it.
 */
export function usePaneOpen(id: CollapsiblePaneId): boolean {
  return useSyncExternalStore(subscribe, () => isOpen(id), () => isOpen(id))
}

/** Open or collapse one pane, and remember the choice in this browser for the next visit. */
export function setPaneOpen(id: CollapsiblePaneId, open: boolean) {
  chosen[id] = open
  try {
    window.localStorage.setItem(STORAGE_PREFIX + id, open ? '1' : '0')
  } catch {
    /* Blocked storage keeps the choice for this page only. */
  }
  listeners.forEach((listener) => listener())
}

/**
 * One side pane of the SQL editor (stardelt-nova#586). Open, it is the card the page had
 * before: a header naming it and a ghost button that collapses it. Collapsed, it is a 40 px
 * rail inside the same card — one button whose label is rotated, so the pane keeps a name a
 * screen reader reads.
 *
 * The open state is remembered per pane in `localStorage`; both reads and the write are
 * guarded, so a blocked store only loses the memory, never the pane.
 */
export default function CollapsiblePane({
  id,
  title,
  side,
  children,
}: {
  id: CollapsiblePaneId
  title: string
  side: 'left' | 'right'
  children: ReactNode
}) {
  const open = usePaneOpen(id)

  if (!open) {
    return (
      // A 40 px rail inside the grid at `lg`; on a phone it is a compact bar above (or below)
      // the editor, where a 40 px-wide column would only eat the editor's width.
      <aside className="card flex min-h-0 flex-col items-center self-start overflow-hidden py-2 lg:self-stretch">
        <button
          type="button"
          onClick={() => setPaneOpen(id, true)}
          aria-label={`Show ${title}`}
          aria-expanded="false"
          className="btn-ghost flex items-center gap-2 border-transparent bg-transparent px-2 py-1 lg:h-full lg:flex-col lg:px-1"
        >
          <span aria-hidden="true">{side === 'left' ? '»' : '«'}</span>
          <span className="label lg:[writing-mode:vertical-rl]">{title}</span>
        </button>
      </aside>
    )
  }

  return (
    <aside className="card flex min-h-0 flex-col overflow-hidden p-3">
      <div className="flex items-center justify-between gap-2 pb-2">
        <h2 className="label">{title}</h2>
        <button
          type="button"
          onClick={() => setPaneOpen(id, false)}
          aria-label={`Collapse ${title}`}
          aria-expanded="true"
          className="btn-ghost px-1.5 py-1"
        >
          <span aria-hidden="true">{side === 'left' ? '«' : '»'}</span>
        </button>
      </div>
      <div className="flex min-h-0 flex-1 flex-col">{children}</div>
    </aside>
  )
}
