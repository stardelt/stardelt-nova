import { useQuery } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { ApiError, api, formatTime, type Connection } from '../api'
import ErrorBox from '../components/ErrorBox'
import { Icon } from '../components/Icon'
import { connectionTypeLabel } from '../connectionTypes'
import { relativeTime } from '../format'
import { isAdmin } from '../roles'

/**
 * Connections: the credentials for systems outside the platform, listed by scope.
 *
 * This page is a ledger, never a vault. The API sends the *names* of the secret fields and
 * no value — by design, the store does not return one — so there is nothing here that could
 * print a credential, and the page has no field to put one in. Creating, rotating and
 * deleting arrive in the next step; the button is disabled until then.
 */

const PERSONAL_NOTE =
  'Only you can use these, from notebooks and the SQL editor. They are never used by pipelines.'
const GROUP_NOTE =
  'Every member of the group can use and rotate these, including pipelines that run as the group.'
const MANAGED_NOTE =
  'Used by platform services only: connection tests, federated catalogs, ingestion. Nobody can read the values.'

/** Name order, so the sections do not reshuffle between two reads of the same list. */
function byName(a: Connection, b: Connection): number {
  return a.name.localeCompare(b.name)
}

export default function Connections() {
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const admin = isAdmin(me.data)
  const list = useQuery({ queryKey: ['connections'], queryFn: api.connections })

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
        <button type="button" className="btn-primary" disabled title="Coming in the next step">
          <Icon name="plus" className="w-4 h-4" />
          New connection
        </button>
      </header>

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
          <ConnectionTable caption="Personal connections" rows={personal} />
        )}
      </Section>

      {list.data !== undefined && groupNames.map(group => (
        <Section key={group} title={`Group: ${group}`} note={GROUP_NOTE}>
          <ConnectionTable
            caption={`Connections of the group ${group}`}
            rows={rows.filter(c => c.scope === 'group' && c.ownerGroup === group).sort(byName)}
          />
        </Section>
      ))}

      {/* Administrators always get the card (the scope exists even while the list is in
          flight); the table needs an answer, so it waits for one. */}
      {admin && (
        <Section title="Managed" note={MANAGED_NOTE}>
          {list.data !== undefined && <ConnectionTable caption="Managed connections" rows={managed} />}
        </Section>
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

function ConnectionTable({ caption, rows }: { caption: string; rows: Connection[] }) {
  return (
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
          {/* Rotate and Delete land here in the next step; the column stays so the rows do not jump. */}
          <th scope="col" className="th px-3 py-2.5"><span className="sr-only">Actions</span></th>
        </tr>
      </thead>
      <tbody>
        {rows.length === 0 && (
          <tr>
            <td className="px-5 py-4 text-text-mute" colSpan={COLUMNS}>Nothing here yet.</td>
          </tr>
        )}
        {rows.map(row => <Row key={row.name} row={row} />)}
      </tbody>
    </table>
  )
}

function Row({ row }: { row: Connection }) {
  const changed = row.lastRotated ? Date.parse(row.lastRotated) : Number.NaN
  const settings = Object.entries(row.config)
  return (
    <tr className="border-t border-border/70 hover:bg-bg-soft/30">
      <td className="td px-5 font-mono text-xs break-all">{row.name}</td>
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
      <td className="td px-3" />
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
