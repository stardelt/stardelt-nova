import { useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent } from 'react'
import {
  api,
  errorMessage,
  type Me,
  type TableDocumentation,
  type TableDocsUpdate,
} from '../api'
import { isAdmin } from '../roles'
import ErrorBox from './ErrorBox'

/** The card's three states, as `useQuery` reports them; the data is optional while a refetch runs. */
export type DocsState = {
  data?: TableDocumentation
  isLoading: boolean
  isError: boolean
  error: unknown
  refetch: () => unknown
}

const MAX_DESCRIPTION = 4000
const MAX_TAGS = 20
const TAG_RULE =
  'Tags are lower-case letters, digits, dash and underscore, up to 32 characters'
const OWNER_RULE =
  'Owner must be a group name: letters, digits, dots, dashes or underscores, up to 64 characters'
const DOMAIN_RULE =
  'Domain must be lower-case letters, digits, dash and underscore, up to 40 characters'

/** The API's tag rule, `^[a-z0-9][a-z0-9_-]{0,31}$`. */
const TAG_RE = /^[a-z0-9][a-z0-9_-]{0,31}$/
/** The API's domain rule, `^[a-z0-9][a-z0-9_-]{0,39}$`. */
const DOMAIN_RE = /^[a-z0-9][a-z0-9_-]{0,39}$/
/** The API's group rule: a letter or digit first, then letters, digits, dots, dashes, underscores. */
const GROUP_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/

/** Group names compare exactly or with Keycloak's leading `/` (its full group path). */
function inGroup(groups: string[], owner: string): string | null {
  return groups.find((g) => g === owner || g.replace(/^\//, '') === owner) ?? null
}

/** The value the owner control starts with. A missing owner is claimed by the first group. */
function initialOwner(groups: string[], owner: string | null, admin: boolean): string {
  if (admin) return owner ?? ''
  if (owner !== null) return inGroup(groups, owner) ?? owner
  return groups[0] ?? ''
}

/** The label a person recognises: Keycloak sends `/data-eng`, the last segment is `data-eng`. */
function groupLabel(group: string): string {
  return group.split('/').filter(Boolean).pop() ?? group
}

function sameList(a: string[], b: string[]): boolean {
  return a.length === b.length && a.every((value, i) => value === b[i])
}

/**
 * "About this table": the table's description, tags, owner and domain, read as plain text
 * and, for someone the API's interim rule allows, edited in place. Every value is plain
 * text — a description is whatever the catalog holds, shown with its line breaks.
 *
 * The card owns its edit form; the answer to a write replaces the read query's data (see
 * `Catalog`), so the card and the Columns tab show the same fresh documentation.
 */
export default function TableAbout({
  ns,
  tbl,
  me,
  docs,
  onSaved,
}: {
  ns: string
  tbl: string
  me?: Me
  docs: DocsState
  onSaved: (doc: TableDocumentation) => void
}) {
  const admin = isAdmin(me)
  const groups = me?.groups ?? []
  const doc = docs.data

  const [editing, setEditing] = useState(false)
  const [description, setDescription] = useState('')
  const [tags, setTags] = useState<string[]>([])
  const [tagDraft, setTagDraft] = useState('')
  const [tagError, setTagError] = useState<string | null>(null)
  const [owner, setOwner] = useState('')
  const [domain, setDomain] = useState('')
  const [saving, setSaving] = useState(false)
  const [saveError, setSaveError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)

  const id = useId()
  const editButton = useRef<HTMLButtonElement>(null)
  const descriptionField = useRef<HTMLTextAreaElement>(null)
  const returnFocus = useRef(false)

  useEffect(() => {
    if (editing) {
      descriptionField.current?.focus()
    } else if (returnFocus.current) {
      returnFocus.current = false
      editButton.current?.focus()
    }
  }, [editing])

  function openEdit() {
    if (!doc) return
    setDescription(doc.description ?? '')
    setTags(doc.tags)
    setTagDraft('')
    setTagError(null)
    setOwner(initialOwner(groups, doc.owner, admin))
    setDomain(doc.domain ?? '')
    setSaveError(null)
    setNotice(null)
    setEditing(true)
  }

  function closeEdit() {
    returnFocus.current = true
    setEditing(false)
    setSaveError(null)
  }

  function addTag() {
    const tag = tagDraft.trim()
    if (!tag) return
    if (!TAG_RE.test(tag)) {
      setTagError(TAG_RULE)
      return
    }
    if (tags.includes(tag)) {
      setTagDraft('')
      setTagError(null)
      return
    }
    if (tags.length >= MAX_TAGS) {
      setTagError(`At most ${MAX_TAGS} tags`)
      return
    }
    setTags([...tags, tag].sort())
    setTagDraft('')
    setTagError(null)
  }

  function onTagKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === 'Enter' || event.key === ',') {
      event.preventDefault()
      addTag()
    }
  }

  const nextDescription = description.trim()
  const descriptionChanged = nextDescription !== (doc?.description ?? '')
  const tagsChanged = doc ? !sameList(tags, doc.tags) : false
  const nextOwner = admin ? owner.trim() : owner
  const ownerChanged = (nextOwner === '' ? null : nextOwner) !== (doc?.owner ?? null)
  // An ownerless table starts the select on the first group so a non-administrator can
  // claim it; that default alone is not an edit, but the claim rides along with any edit.
  const ownerTouched = owner !== (doc ? initialOwner(groups, doc.owner, admin) : '')
  const nextDomain = domain.trim()
  const domainChanged = (nextDomain === '' ? null : nextDomain) !== (doc?.domain ?? null)

  const draftTag = tagDraft.trim()
  // The current owner is normally one of the person's groups; keep it selectable if a
  // rename left it out, so the control never shows a blank value.
  const ownerOptions = owner !== '' && !groups.includes(owner) ? [owner, ...groups] : groups
  const ownerInvalid = admin
    ? owner.trim() !== '' && !GROUP_RE.test(owner.trim())
    : (owner === '' && doc?.canEdit === true) || (owner !== '' && !groups.includes(owner))
  const domainInvalid = domain.trim() !== '' && !DOMAIN_RE.test(domain.trim())
  const tagsInvalid = (draftTag !== '' && !TAG_RE.test(draftTag)) || tags.length > MAX_TAGS
  const changed = descriptionChanged || tagsChanged || ownerTouched || domainChanged
  const canSave = changed && !ownerInvalid && !domainInvalid && !tagsInvalid && !saving

  async function save(event: FormEvent) {
    event.preventDefault()
    if (!doc || !canSave) return

    // A tag left in the input is part of what the person typed; add it first, or refuse it.
    let finalTags = tags
    if (draftTag !== '') {
      if (!TAG_RE.test(draftTag)) {
        setTagError(TAG_RULE)
        return
      }
      if (!finalTags.includes(draftTag)) {
        if (finalTags.length >= MAX_TAGS) {
          setTagError(`At most ${MAX_TAGS} tags`)
          return
        }
        finalTags = [...finalTags, draftTag].sort()
        setTags(finalTags)
      }
      setTagDraft('')
      setTagError(null)
    }

    const body: TableDocsUpdate = {}
    if (descriptionChanged) body.description = nextDescription === '' ? null : nextDescription
    if (!sameList(finalTags, doc.tags)) body.tags = finalTags
    if (ownerChanged) body.owner = nextOwner === '' ? null : nextOwner
    if (domainChanged) body.domain = nextDomain === '' ? null : nextDomain

    setSaving(true)
    setSaveError(null)
    try {
      const answer = await api.putTableDocs(ns, tbl, body)
      onSaved(answer)
      setNotice('Saved.')
      returnFocus.current = true
      setEditing(false)
    } catch (error) {
      setSaveError(errorMessage(error))
    } finally {
      setSaving(false)
    }
  }

  return (
    <section className="card p-5 min-w-0" aria-labelledby={`${id}-title`}>
      <div className="flex flex-wrap items-start justify-between gap-3">
        <h2 id={`${id}-title`} className="label">About this table</h2>
        {doc?.canEdit && !editing && (
          <button
            ref={editButton}
            type="button"
            aria-label="Edit about this table"
            className="btn-ghost py-1 px-2.5 text-xs"
            onClick={openEdit}
          >
            Edit
          </button>
        )}
      </div>

      {docs.isLoading && !doc && <p className="text-sm text-text-mute mt-3">Loading…</p>}
      {docs.isError && !doc && (
        <div className="mt-3">
          <ErrorBox error={docs.error} onRetry={() => void docs.refetch()} />
        </div>
      )}

      {doc && !editing && (
        <>
          <p className="mt-3 text-sm text-text-dim whitespace-pre-wrap break-words">
            {doc.description ?? <span className="text-text-mute">No description yet.</span>}
          </p>
          {!doc.description && doc.canEdit && (
            <p className="mt-1 text-xs text-text-mute">Say what the table holds and where it comes from.</p>
          )}

          <div className="mt-4 grid gap-4 sm:grid-cols-3">
            <div className="min-w-0">
              <div className="label mb-1">Tags</div>
              {doc.tags.length > 0 ? (
                <ul className="flex flex-wrap gap-1.5">
                  {doc.tags.map((tag) => (
                    <li
                      key={tag}
                      className="badge min-w-0 max-w-full whitespace-normal break-words border-border bg-bg-soft text-text-dim"
                    >
                      {tag}
                    </li>
                  ))}
                </ul>
              ) : (
                <span className="text-sm text-text-mute">No tags.</span>
              )}
            </div>
            <div className="min-w-0">
              <div className="label mb-1">Owner</div>
              <div className="text-sm text-text-dim break-words">
                {doc.owner ?? <span className="text-text-mute">No owner</span>}
              </div>
            </div>
            <div className="min-w-0">
              <div className="label mb-1">Domain</div>
              <div className="text-sm text-text-dim break-words">
                {doc.domain ?? <span className="text-text-mute">No domain</span>}
              </div>
            </div>
          </div>

          {!doc.canEdit && <p className="mt-3 text-text-mute text-xs break-words">{doc.editRule}</p>}
        </>
      )}

      {notice && <p role="status" className="mt-3 text-sm text-ok break-words">{notice}</p>}

      {doc && editing && (
        <form className="mt-4 space-y-4" onSubmit={save} onKeyDown={(event) => {
          if (event.key === 'Escape') {
            event.preventDefault()
            closeEdit()
          }
        }}>
          <div>
            <label htmlFor={`${id}-description`} className="label block mb-1.5">Description</label>
            <textarea
              ref={descriptionField}
              id={`${id}-description`}
              className="input min-h-[7rem] resize-y"
              maxLength={MAX_DESCRIPTION}
              value={description}
              onChange={(event) => setDescription(event.target.value)}
            />
            <div className="mt-1 text-right text-xs text-text-mute">{description.length} / {MAX_DESCRIPTION}</div>
          </div>

          <div>
            <label htmlFor={`${id}-tags`} className="label block mb-1.5">Tags</label>
            <div className="input flex flex-wrap items-center gap-1.5 py-2">
              {tags.map((tag) => (
                <span key={tag} className="badge min-w-0 max-w-full border-border bg-bg-soft text-text-dim">
                  <span className="break-words">{tag}</span>
                  <button
                    type="button"
                    aria-label={`Remove tag ${tag}`}
                    className="text-text-mute hover:text-text"
                    onClick={() => setTags(tags.filter((value) => value !== tag))}
                  >
                    ×
                  </button>
                </span>
              ))}
              <input
                id={`${id}-tags`}
                className="min-w-[8rem] flex-1 border-0 bg-transparent px-1 py-0.5 text-sm text-text outline-hidden placeholder:text-text-mute"
                value={tagDraft}
                onChange={(event) => {
                  setTagDraft(event.target.value)
                  if (tagError) setTagError(null)
                }}
                onKeyDown={onTagKeyDown}
                onBlur={addTag}
                placeholder={tags.length === 0 ? 'Add a tag…' : undefined}
              />
            </div>
            {tagError && <p role="alert" className="mt-1 text-xs text-bad break-words">{tagError}</p>}
            <p className="mt-1 text-xs text-text-mute">Press Enter or comma to add a tag. {MAX_TAGS} at most.</p>
          </div>

          <div className="grid gap-4 sm:grid-cols-2">
            <div className="min-w-0">
              <label htmlFor={`${id}-owner`} className="label block mb-1.5">Owner</label>
              {admin ? (
                <input
                  id={`${id}-owner`}
                  className="input"
                  value={owner}
                  onChange={(event) => setOwner(event.target.value)}
                  placeholder="group-name"
                />
              ) : (
                <select
                  id={`${id}-owner`}
                  className="input"
                  value={owner}
                  onChange={(event) => setOwner(event.target.value)}
                >
                  {ownerOptions.map((group) => (
                    <option key={group} value={group}>{groupLabel(group)}</option>
                  ))}
                </select>
              )}
              {admin && owner.trim() !== '' && !GROUP_RE.test(owner.trim()) && (
                <p className="mt-1 text-xs text-bad break-words">{OWNER_RULE}</p>
              )}
              {admin && owner.trim() === '' && (
                <p className="mt-1 text-xs text-text-mute">Leave empty to remove the owner.</p>
              )}
              {!admin && doc.owner === null && (
                <p className="mt-1 text-xs text-text-mute">The first editor claims the table for one of their groups.</p>
              )}
            </div>

            <div className="min-w-0">
              <label htmlFor={`${id}-domain`} className="label block mb-1.5">Domain</label>
              <input
                id={`${id}-domain`}
                className="input"
                value={domain}
                onChange={(event) => setDomain(event.target.value)}
                placeholder="sales"
              />
              {domain.trim() !== '' && !DOMAIN_RE.test(domain.trim()) && (
                <p className="mt-1 text-xs text-bad break-words">{DOMAIN_RULE}</p>
              )}
            </div>
          </div>

          {saveError && (
            <div role="alert" className="rounded-[10px] border border-red-500/40 bg-red-500/10 p-3 text-sm text-bad whitespace-pre-wrap break-words">
              {saveError}
            </div>
          )}

          <div className="flex flex-wrap gap-2">
            <button type="submit" className="btn-primary" disabled={!canSave}>
              {saving ? 'Saving…' : 'Save'}
            </button>
            <button type="button" className="btn-ghost" onClick={closeEdit}>Cancel</button>
          </div>
        </form>
      )}
    </section>
  )
}
