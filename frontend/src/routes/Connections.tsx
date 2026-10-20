import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useRef, useState, type ReactNode } from 'react'
import { ApiError, api, formatTime, type Connection } from '../api'
import {
  BreakGlassDialog,
  DeleteConnectionDialog,
  NewConnectionDialog,
  RotateConnectionDialog,
} from '../components/ConnectionDialogs'
import ErrorBox from '../components/ErrorBox'
import { Icon } from '../components/Icon'
import { connectionTypeLabel } from '../connectionTypes'
import { relativeTime } from '../format'
import { isAdmin } from '../roles'

/**
 * Connections: the credentials for systems outside the platform, listed by scope, and the
 * four dialogs that create, rotate, delete and (for an administrator, with a reason) read
 * one group connection once. An administrator's break-glass read leaves the owning group a
 * notice above its table, with a Rotate now action; a rotation clears the notice.
 *
 * This page is a ledger, never a vault. The API sends the *names* of the secret fields and
 * no value — by design, the store does not return one — so there is nothing here that could
 * print a credential. The one exception is the answer to a break-glass read: it lives only
 * in the open dialog's state, masked until Show, and is dropped after sixty seconds.
 */

type OpenDialog =
  | { kind: 'new' }
  | { kind: 'rotate'; connection: Connection }
  | { kind: 'delete'; connection: Connection }
  | { kind: 'break-glass'; connection: Connection; nonce: number }

/**
 * What each scope's sentence may claim: only what works today (stardelt-nova#202). A
 * connection is read from a Python notebook; no SQL statement, Spark notebook, pipeline
 * task or catalog consumes one yet. The ticket that builds one of those changes the
 * sentence here in the same pull request.
 */
const PERSONAL_NOTE =
  'Only you can see and use these, from a Python notebook. They are never used by pipelines or SQL.'
const GROUP_NOTE =
  'Every member of the group can use and rotate these from a Python notebook.'
const MANAGED_NOTE =
  'For platform services only. Nobody can read the values.'

/** Name order, so the sections do not reshuffle between two reads of the same list. */
function byName(a: Connection, b: Connection): number {
  return a.name.localeCompare(b.name)
}

export default function Connections() {
  const queryClient = useQueryClient()
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const admin = isAdmin(me.data)
  const list = useQuery({ queryKey: ['connections'], queryFn: api.connections })
  const [dialog, setDialog] = useState<OpenDialog | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const breakGlassNonce = useRef(0)

  /** Close the dialog, say what happened, and read the list again. */
  function done(message: string) {
    setDialog(null)
    setNotice(message)
    void queryClient.invalidateQueries({ queryKey: ['connections'] })
  }

  /** A break-glass read leaves the group a notice; closing the dialog reads the list again. */
  function closeBreakGlass() {
    // The dialog closes itself and clears the answer before it goes quiet; the element
    // stays mounted (a closed `<dialog>` is inert) until another dialog replaces it.
    void queryClient.invalidateQueries({ queryKey: ['connections'] })
  }

  /** A new nonce remounts the closed dialog, so its one-shot listener shows it again. */
  function openBreakGlass(connection: Connection) {
    breakGlassNonce.current += 1
    setDialog({ kind: 'break-glass', connection, nonce: breakGlassNonce.current })
  }

  const rows = list.data?.connections ?? []
  const personal = rows.filter(c => c.scope === 'personal').sort(byName)
  const groupNames = [...new Set(
    rows.filter(c => c.scope === 'group').map(c => c.ownerGroup).filter((g): g is string => !!g),
  )].sort((a, b) => a.localeCompare(b))
  const managed = rows.filter(c => c.scope === 'managed').sort(byName)
  const failed = list.isError && !list.data
  // Without a configured store every connection route answers 503 (stardelt-nova#171): the
  // sentence is a fact about the installation, not a failure a Retry could fix.
  const storeMissing = failed && list.error instanceof ApiError && list.error.status === 503

  return (
    <div className="max-w-6xl space-y-5">
      <header className="flex flex-wrap items-start justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-2xl font-bold tracking-tight">Connections</h1>
          <p className="text-text-dim text-sm mt-1">
            Credentials for systems outside the platform. The values are write-only: nobody can read them back here.
          </p>
        </div>
        <button type="button" className="btn-primary" onClick={() => setDialog({ kind: 'new' })}>
          <Icon name="plus" className="w-4 h-4" />
          New connection
        </button>
      </header>

      {notice && <p role="status" className="text-sm text-ok break-words">{notice}</p>}

      <Section title="Personal" note={PERSONAL_NOTE}>
        {list.isLoading && <p className="px-5 pb-5 text-sm text-text-mute">Loading connections…</p>}
        {failed && (
          <div className="px-5 pb-5 space-y-2">
            <ErrorBox error={list.error} onRetry={storeMissing ? undefined : () => void list.refetch()} />
            {storeMissing && (
              <p className="text-sm text-text-mute">
                An administrator has to set up the secret store for this installation.
              </p>
            )}
          </div>
        )}
        {list.data !== undefined && (
          <ConnectionTable
            caption="Personal connections"
            rows={personal}
            admin={admin}
            onRotate={(connection) => setDialog({ kind: 'rotate', connection })}
            onDelete={(connection) => setDialog({ kind: 'delete', connection })}
            onBreakGlass={openBreakGlass}
          />
        )}
      </Section>

      {list.data !== undefined && groupNames.map(group => (
        <Section key={group} title={`Group: ${group}`} note={GROUP_NOTE}>
          <ConnectionTable
            caption={`Connections of the group ${group}`}
            rows={rows.filter(c => c.scope === 'group' && c.ownerGroup === group).sort(byName)}
            admin={admin}
            onRotate={(connection) => setDialog({ kind: 'rotate', connection })}
            onDelete={(connection) => setDialog({ kind: 'delete', connection })}
            onBreakGlass={openBreakGlass}
          />
        </Section>
      ))}

      {/* Administrators always get the card (the scope exists even while the list is in
          flight); the table needs an answer, so it waits for one. */}
      {admin && (
        <Section title="Managed" note={MANAGED_NOTE}>
          {list.data !== undefined && (
            <ConnectionTable
              caption="Managed connections"
              rows={managed}
              admin={admin}
              onRotate={(connection) => setDialog({ kind: 'rotate', connection })}
              onDelete={(connection) => setDialog({ kind: 'delete', connection })}
              onBreakGlass={openBreakGlass}
            />
          )}
        </Section>
      )}

      {dialog?.kind === 'new' && (
        <NewConnectionDialog
          me={me.data}
          onClose={() => setDialog(null)}
          onCreated={(name) => done(`Created ${name}.`)}
        />
      )}
      {dialog?.kind === 'rotate' && (
        <RotateConnectionDialog
          connection={dialog.connection}
          onClose={() => setDialog(null)}
          onRotated={(name) => done(`Rotated ${name}.`)}
        />
      )}
      {dialog?.kind === 'delete' && (
        <DeleteConnectionDialog
          connection={dialog.connection}
          onClose={() => setDialog(null)}
          onDeleted={(name) => done(`Deleted ${name}.`)}
        />
      )}
      {dialog?.kind === 'break-glass' && (
        <BreakGlassDialog
          key={dialog.nonce}
          connection={dialog.connection}
          onClose={closeBreakGlass}
        />
      )}
    </div>
  )
}

function Section({ title, note, children }: { title: string; note: string; children: ReactNode }) {
  return (
    <section className="card overflow-hidden">
      <div className="px-5 pt-5 pb-3">
        <h2 className="text-lg font-semibold">{title}</h2>
        <p className="text-text-mute text-sm mt-1">{note}</p>
      </div>
      {/* The table scrolls inside the card, so a long name never widens the page (rule U4).
          `relative` makes this the containing block for the table's `sr-only` text, which is
          absolutely positioned: without it that text anchors to the page and a wide table
          widens the document even though it is clipped here. */}
      <div className="relative overflow-x-auto">{children}</div>
    </section>
  )
}

const COLUMNS = 7
/** A notice shows at most this many characters of each API-supplied text; `title` holds it all. */
const NOTICE_LIMIT = 200

/** What a person sees of an API-supplied string, with the full text in `title`. */
function clipped(value: string): { shown: string; full: string } {
  return {
    shown: value.length > NOTICE_LIMIT ? value.slice(0, NOTICE_LIMIT) : value,
    full: value,
  }
}

/** The read time as a date a person reads; the raw value only if the API sent something else. */
function noticeDate(at: string): string {
  const ms = Date.parse(at)
  return Number.isFinite(ms) ? formatTime(ms) : at
}

function ConnectionTable({
  caption,
  rows,
  admin,
  onRotate,
  onDelete,
  onBreakGlass,
}: {
  caption: string
  rows: Connection[]
  admin: boolean
  onRotate: (connection: Connection) => void
  onDelete: (connection: Connection) => void
  onBreakGlass: (connection: Connection) => void
}) {
  return (
    <>
      {/* A break-glass notice belongs to the owning group and sits above its table. */}
      {rows.map(row => row.scope === 'group' && row.breakGlass
        ? <BreakGlassBanner key={`notice-${row.name}`} notice={row.breakGlass} onRotate={() => onRotate(row)} />
        : null)}
      <table className="w-full text-sm">
        <caption className="sr-only">{caption}</caption>
        <thead className="bg-bg-soft/50">
          <tr>
            <th scope="col" className="th px-5 py-2.5">Name</th>
            <th scope="col" className="th px-3 py-2.5">Type</th>
            <th scope="col" className="th px-3 py-2.5">Settings</th>
            <th scope="col" className="th px-3 py-2.5">Secret fields</th>
            <th scope="col" className="th px-3 py-2.5">Changed</th>
            <th scope="col" className="th px-3 py-2.5">Status</th>
            <th scope="col" className="th px-3 py-2.5"><span className="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {rows.length === 0 && (
            <tr>
              <td className="px-5 py-4 text-text-mute" colSpan={COLUMNS}>Nothing here yet.</td>
            </tr>
          )}
          {rows.map(row => (
            <Row
              key={row.name}
              row={row}
              admin={admin}
              onRotate={() => onRotate(row)}
              onDelete={() => onDelete(row)}
              onBreakGlass={() => onBreakGlass(row)}
            />
          ))}
        </tbody>
      </table>
    </>
  )
}

/** The owning group's notice: an administrator read the credential, and why. */
function BreakGlassBanner({
  notice,
  onRotate,
}: {
  notice: NonNullable<Connection['breakGlass']>
  onRotate: () => void
}) {
  const by = clipped(notice.by)
  const reason = clipped(notice.reason)
  return (
    <div
      role="status"
      className="mx-5 mt-4 mb-1 rounded-[10px] border border-amber-400/40 bg-amber-400/10 p-3 text-sm text-text-dim"
    >
      <div className="flex items-start gap-2">
        <Icon name="key" className="mt-0.5 h-4 w-4 shrink-0" />
        <div className="min-w-0 space-y-2">
          <p className="break-words">
            Read by <span title={by.full}>{by.shown}</span> on {noticeDate(notice.at)}:{' '}
            <span title={reason.full}>{reason.shown}</span>. Consider rotating the credential.
          </p>
          <button
            type="button"
            className="btn-ghost py-1 px-2 text-xs"
            onClick={onRotate}
          >
            Rotate now
          </button>
        </div>
      </div>
    </div>
  )
}

/** A copy that fails leaves the text selectable and says what to do; the clipboard may be refused. */
const COPY_FAILED = 'Copy failed: select the text and copy it'

/**
 * The lines a person pastes into a Python notebook, built from the row's own data as plain
 * text. They name the connection and its first secret field; the page never holds a value,
 * so nothing here could print one. A name or group is data, never markup: it reaches the
 * page as a text node in `<pre>`.
 */
function notebookSnippet(row: Connection): string {
  const group = row.scope === 'group' ? `, group="${row.ownerGroup ?? ''}"` : ''
  const lines = ['import stardelt', `c = stardelt.connection("${row.name}"${group})`]
  const first = row.secretKeys[0]
  if (first) lines.push(`c.value("${first}")`)
  return lines.join('\n')
}

/**
 * How to use the connection, folded away until asked for: the snippet, its Copy button and
 * nothing else. A managed connection has no Python reader, so it never renders this.
 */
function UseDisclosure({ row }: { row: Connection }) {
  const [copyFailed, setCopyFailed] = useState(false)
  const snippet = notebookSnippet(row)

  async function copy() {
    setCopyFailed(false)
    try {
      if (!navigator.clipboard) throw new Error('no clipboard')
      await navigator.clipboard.writeText(snippet)
    } catch {
      setCopyFailed(true)
    }
  }

  return (
    <details className="mt-1">
      <summary className="cursor-pointer whitespace-nowrap text-xs font-normal text-accent">Use in a notebook</summary>
      {/* The snippet keeps a readable width and the table scrolls inside its card: a phone
          shows the three lines, not one word per line (rule U4 concerns the page, not the card). */}
      <div className="mt-2 space-y-1">
        <pre className="min-w-[16rem] whitespace-pre-wrap wrap-anywhere rounded-[8px] border border-border/70 bg-bg-soft/50 p-2 font-mono text-[11px] select-all">{snippet}</pre>
        <button
          type="button"
          className="btn-ghost py-1 px-2 text-xs"
          aria-label={`Copy the snippet for ${row.name}`}
          onClick={() => void copy()}
        >
          Copy
        </button>
        {copyFailed && <p role="alert" className="text-xs text-bad break-words">{COPY_FAILED}</p>}
      </div>
    </details>
  )
}

function Row({ row, admin, onRotate, onDelete, onBreakGlass }: {
  row: Connection
  admin: boolean
  onRotate: () => void
  onDelete: () => void
  onBreakGlass: () => void
}) {
  const changed = row.lastRotated ? Date.parse(row.lastRotated) : Number.NaN
  const settings = Object.entries(row.config)
  return (
    <tr className="border-t border-border/70 hover:bg-bg-soft/30">
      <td className="td px-5">
        <div className="font-mono text-xs break-all">{row.name}</div>
        {row.scope !== 'managed' && <UseDisclosure row={row} />}
      </td>
      <td className="td px-3 whitespace-nowrap">{connectionTypeLabel(row.type)}</td>
      <td className="td px-3">
        {settings.length === 0 ? (
          <span className="text-text-mute">—</span>
        ) : (
          <div className="font-mono text-xs break-all space-y-0.5">
            {settings.map(([key, value]) => <div key={key}>{key}={value}</div>)}
          </div>
        )}
      </td>
      <td className="td px-3 font-mono text-xs break-all">
        {row.secretKeys.length === 0
          ? <span className="text-text-mute">—</span>
          : row.secretKeys.join(', ')}
      </td>
      <td
        className="td px-3 text-text-dim text-xs whitespace-nowrap"
        title={Number.isFinite(changed) ? formatTime(changed) : undefined}
      >
        {Number.isFinite(changed) ? relativeTime(changed) : '—'}
      </td>
      <td className="td px-3"><Status row={row} /></td>
      <td className="td px-3">
        <div className="flex flex-wrap items-center justify-end gap-2">
          <button
            type="button"
            className="btn-ghost py-1 px-2 text-xs"
            aria-label={`Rotate ${row.name}`}
            onClick={onRotate}
          >
            Rotate
          </button>
          <button
            type="button"
            className="btn-ghost py-1 px-2 text-xs text-bad"
            aria-label={`Delete ${row.name}`}
            onClick={onDelete}
          >
            Delete
          </button>
          {admin && row.scope === 'group' && (
            <div className="border-l border-border/70 pl-2">
              <button
                type="button"
                className="btn-ghost py-1 px-2 text-xs"
                aria-label={`Break-glass read ${row.name}`}
                onClick={onBreakGlass}
              >
                Break-glass read
              </button>
            </div>
          )}
        </div>
      </td>
    </tr>
  )
}

/** A status is an icon *and* a word, so it survives colour blindness and a monochrome screen. */
function Status({ row }: { row: Connection }) {
  if (row.scope === 'personal') {
    return <span className="badge border-border bg-bg-soft text-text-dim">Stored</span>
  }
  const sync = row.sync
  if (!sync) return <span className="text-text-mute text-xs">Unknown</span>
  const detail = sync.message || sync.reason
  return (
    <div className="space-y-0.5">
      {sync.ready ? (
        <span className="badge border-emerald-400/40 bg-emerald-400/10 text-ok">
          <span aria-hidden>✓</span>Ready
        </span>
      ) : (
        <span className="badge border-red-400/40 bg-red-400/10 text-bad">
          <span aria-hidden>✕</span>Not ready
        </span>
      )}
      {!sync.ready && detail && <p className="text-text-mute text-xs break-words">{detail}</p>}
    </div>
  )
}
