import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { ApiError, api, errorMessage, type Destination } from '../api'
import ErrorBox from '../components/ErrorBox'
import { Icon } from '../components/Icon'

const EMAIL_NOTE = 'Email needs the platform’s SMTP relay; it is not configured yet.'
const NO_GROUP = 'You are not in a group, so you cannot add a destination.'
const DEMO_NOTE = 'The demo has no state store, so notification destinations are not available.'

/** The address a person reads: the webhook URL, or the recipients joined. */
const address = (row: Destination) =>
  row.kind === 'email' ? (row.config.recipients ?? []).join(', ') : (row.config.url ?? '')

/** Name order, so a section does not reshuffle between two reads of the same list. */
const byName = (a: Destination, b: Destination) => a.name.localeCompare(b.name)

/**
 * Notification destinations (stardelt-nova#273, UI slice): a named webhook or email
 * address owned by one group. The API decides which groups the caller sees and may write
 * to; this page only lists, creates, tests one webhook and deletes.
 */
export default function Destinations() {
  const queryClient = useQueryClient()
  const me = useQuery({ queryKey: ['me'], queryFn: api.me })
  const demo = me.data?.demo === true
  // Demo mode has no state store, so the list itself would answer 503: do not ask.
  const list = useQuery({
    queryKey: ['destinations'],
    queryFn: api.destinations,
    enabled: me.data !== undefined && me.data.demo !== true,
  })
  const [creating, setCreating] = useState(false)
  const [toDelete, setToDelete] = useState<Destination | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const groups = me.data?.groups ?? []
  // `503` is a fact about the installation (no `NOVA_DATABASE_URL`), not a failure a Retry
  // could fix: show the API's own sentence and offer no form that could only fail.
  const storeMissing = list.error instanceof ApiError && list.error.status === 503
  const rows = list.data ?? []
  const groupNames = [...new Set(rows.map((r) => r.owner_group))].sort((a, b) => a.localeCompare(b))

  function refresh(message: string) {
    setCreating(false)
    setNotice(message)
    void queryClient.invalidateQueries({ queryKey: ['destinations'] })
  }

  const header = (
    <header className="flex flex-wrap items-start justify-between gap-4">
      <div className="min-w-0">
        <h1 className="text-2xl font-bold tracking-tight">Destinations</h1>
        <p className="mt-1 text-sm text-text-dim">
          Named webhook or email addresses your group can send notifications to.
        </p>
      </div>
      {!demo && !storeMissing && groups.length > 0 && !creating && (
        <button type="button" className="btn-primary" onClick={() => setCreating(true)}>
          <Icon name="plus" className="h-4 w-4" />
          New destination
        </button>
      )}
    </header>
  )

  // Demo mode has no state store: say so and offer nothing that could only answer 503.
  if (demo) {
    return <div className="max-w-6xl space-y-5">{header}<p className="text-sm text-text-dim">{DEMO_NOTE}</p></div>
  }

  return (
    <div className="max-w-6xl space-y-5">
      {header}
      {notice && <p role="status" className="text-sm text-ok break-words">{notice}</p>}
      {storeMissing && <ErrorBox error={list.error} />}
      {!storeMissing && me.data !== undefined && groups.length === 0 && (
        <p className="text-sm text-text-mute">{NO_GROUP}</p>
      )}
      {creating && (
        <NewDestinationForm
          groups={groups}
          onClose={() => setCreating(false)}
          onCreated={(name) => refresh(`Created “${name}”.`)}
        />
      )}
      {!storeMissing && list.isLoading && <p className="text-sm text-text-mute">Loading destinations…</p>}
      {!storeMissing && list.isError && <ErrorBox error={list.error} onRetry={() => void list.refetch()} />}
      {!storeMissing && list.data !== undefined && rows.length === 0 && (
        <p className="text-sm text-text-mute">
          No destinations yet. Add one when a group wants to hear about something.
        </p>
      )}
      {!storeMissing && groupNames.map((group) => (
        <section key={group} className="card overflow-hidden">
          <div className="px-5 pt-5 pb-3">
            <h2 className="text-lg font-semibold break-words">Group: {group}</h2>
            {rows.some((r) => r.owner_group === group && r.kind === 'email') && (
              <p className="mt-1 text-sm text-text-mute">{EMAIL_NOTE}</p>
            )}
          </div>
          <div className="relative overflow-x-auto">
            <table className="w-full text-sm">
              <caption className="sr-only">Destinations of the group {group}</caption>
              <thead className="bg-bg-soft/50">
                <tr>
                  <th scope="col" className="th px-5 py-2.5">Name</th>
                  <th scope="col" className="th px-3 py-2.5">Kind</th>
                  <th scope="col" className="th px-3 py-2.5">Address</th>
                  <th scope="col" className="th px-3 py-2.5"><span className="sr-only">Actions</span></th>
                </tr>
              </thead>
              <tbody>
                {rows.filter((r) => r.owner_group === group).sort(byName).map((row) => (
                  <DestinationRow key={row.id} row={row} onDelete={() => setToDelete(row)} />
                ))}
              </tbody>
            </table>
          </div>
        </section>
      ))}
      {toDelete && (
        <DeleteDestinationDialog
          destination={toDelete}
          onClose={() => setToDelete(null)}
          onDeleted={(name) => { setToDelete(null); refresh(`Deleted “${name}”.`) }}
        />
      )}
    </div>
  )
}

function DestinationRow({ row, onDelete }: { row: Destination; onDelete: () => void }) {
  const [testing, setTesting] = useState(false)
  const [result, setResult] = useState<{ ok: boolean; detail: string } | null>(null)

  async function test() {
    setTesting(true)
    setResult(null)
    try {
      const answer = await api.testDestination(row.id)
      setResult({ ok: answer.ok, detail: answer.ok ? 'Delivered.' : answer.detail })
    } catch (e) {
      setResult({ ok: false, detail: errorMessage(e) })
    } finally {
      setTesting(false)
    }
  }

  return (
    <tr className="border-t border-border/70 hover:bg-bg-soft/30">
      <td className="td px-5 font-mono text-xs break-all">{row.name}</td>
      <td className="td px-3"><span className="badge border-border bg-bg-soft text-text-dim">{row.kind}</span></td>
      <td className="td px-3 font-mono text-xs break-all">
        {address(row) || <span className="text-text-mute">—</span>}
      </td>
      <td className="td px-3">
        <div className="flex flex-wrap items-center justify-end gap-2">
          {row.kind === 'webhook' && (
            <button type="button" className="btn-ghost px-2 py-1 text-xs" disabled={testing} onClick={() => void test()}>
              {testing ? 'Testing…' : 'Test'}
            </button>
          )}
          <button type="button" className="btn-ghost px-2 py-1 text-xs text-bad" onClick={onDelete}>
            Delete “{row.name}”
          </button>
        </div>
        {result && (
          <p role="status" className={`mt-1 text-right text-xs break-words ${result.ok ? 'text-ok' : 'text-bad'}`}>
            {result.detail}
          </p>
        )}
      </td>
    </tr>
  )
}

/** One labelled control; the form's `<label>` and its field, so every input is named. */
function Field({ id, label, children }: { id: string; label: string; children: ReactNode }) {
  return (
    <div>
      <label htmlFor={id} className="label mb-1.5 block">{label}</label>
      {children}
    </div>
  )
}

/** The inline card the New destination button reveals; no modal, so the list stays in view. */
function NewDestinationForm({ groups, onClose, onCreated }: {
  groups: string[]
  onClose: () => void
  onCreated: (name: string) => void
}) {
  const nameId = useId()
  const groupId = useId()
  const kindId = useId()
  const addressId = useId()
  const [ownerGroup, setOwnerGroup] = useState(groups[0] ?? '')
  const [name, setName] = useState('')
  const [kind, setKind] = useState<'webhook' | 'email'>('webhook')
  const [url, setUrl] = useState('')
  const [recipients, setRecipients] = useState('')
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function submit(event: FormEvent) {
    event.preventDefault()
    setError(null)
    const value = name.trim()
    if (value.length < 1 || value.length > 64) return setError('A name is 1 to 64 characters.')
    let config: unknown
    if (kind === 'webhook') {
      const trimmed = url.trim()
      if (!/^https?:\/\//i.test(trimmed)) return setError('A webhook address starts with http:// or https://.')
      config = { url: trimmed }
    } else {
      const list = recipients.split(',').map((r) => r.trim()).filter(Boolean)
      if (list.length === 0) return setError('An email destination needs at least one recipient.')
      config = { recipients: list }
    }
    setSaving(true)
    try {
      await api.createDestination({ owner_group: ownerGroup, name: value, kind, config })
      onCreated(value)
    } catch (e) {
      setError(errorMessage(e))
    } finally {
      setSaving(false)
    }
  }

  return (
    <form onSubmit={submit} className="card space-y-4 p-5">
      <h2 className="text-lg font-semibold">New destination</h2>
      <div className="grid gap-4 sm:grid-cols-2">
        <Field id={groupId} label="Owner group">
          <select id={groupId} className="input w-full" value={ownerGroup} onChange={(e) => setOwnerGroup(e.target.value)}>
            {groups.map((g) => <option key={g} value={g}>{g}</option>)}
          </select>
        </Field>
        <Field id={nameId} label="Name">
          <input id={nameId} className="input w-full" value={name} maxLength={64} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field id={kindId} label="Kind">
          <select
            id={kindId}
            className="input w-full"
            value={kind}
            onChange={(e) => setKind(e.target.value === 'email' ? 'email' : 'webhook')}
          >
            <option value="webhook">webhook</option>
            <option value="email">email</option>
          </select>
        </Field>
        <Field id={addressId} label={kind === 'webhook' ? 'URL' : 'Recipients'}>
          {kind === 'webhook' ? (
            <input
              id={addressId}
              type="url"
              className="input w-full"
              placeholder="https://hooks.example/…"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />
          ) : (
            <input
              id={addressId}
              className="input w-full"
              placeholder="on-call@example.com, data@example.com"
              value={recipients}
              onChange={(e) => setRecipients(e.target.value)}
            />
          )}
        </Field>
      </div>
      {error && <p role="alert" className="text-sm text-bad break-words">{error}</p>}
      <div className="flex items-center gap-2">
        <button type="submit" className="btn-primary" disabled={saving}>{saving ? 'Saving…' : 'Create destination'}</button>
        <button type="button" className="btn-ghost" onClick={onClose}>Cancel</button>
      </div>
    </form>
  )
}

/** A native `<dialog>`, like `ComputeDialog`: the browser holds the focus and Escape closes it. */
function DeleteDestinationDialog({ destination, onClose, onDeleted }: {
  destination: Destination
  onClose: () => void
  onDeleted: (name: string) => void
}) {
  const titleId = useId()
  const dialog = useRef<HTMLDialogElement>(null)
  const [deleting, setDeleting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const element = dialog.current
    if (!element) return
    if (!element.open) element.showModal()
    return () => {
      if (element.open) element.close()
    }
  }, [])

  async function remove() {
    setDeleting(true)
    setError(null)
    try {
      await api.deleteDestination(destination.id)
      onDeleted(destination.name)
    } catch (e) {
      setError(errorMessage(e))
    } finally {
      setDeleting(false)
    }
  }

  return (
    <dialog
      ref={dialog}
      aria-labelledby={titleId}
      onCancel={(event) => { event.preventDefault(); onClose() }}
      className="m-auto w-[min(30rem,calc(100vw_-_2rem))] max-w-none rounded-[14px] border border-border bg-bg-elev p-0 text-text shadow-2xl backdrop:bg-black/60"
    >
      <div className="space-y-3 p-5">
        <h2 id={titleId} className="text-lg font-semibold break-words">Delete “{destination.name}”?</h2>
        <p className="text-sm text-text-dim">Anything that sends to this destination will stop.</p>
        {error && <p role="alert" className="text-sm text-bad break-words">{error}</p>}
        <div className="flex items-center justify-end gap-2">
          <button type="button" className="btn-ghost" onClick={onClose}>Cancel</button>
          <button type="button" className="btn-primary" disabled={deleting} onClick={() => void remove()}>
            {deleting ? 'Deleting…' : 'Delete'}
          </button>
        </div>
      </div>
    </dialog>
  )
}
