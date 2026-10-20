import { useEffect, useId, useRef, useState, type FormEvent, type ReactNode } from 'react'
import { api, errorMessage, type Connection, type Me } from '../api'
import { CONNECTION_TYPES, type Field } from '../connectionTypes'
import { isAdmin } from '../roles'

/**
 * The three dialogs of the Connections page: create, rotate and delete.
 *
 * A secret value lives only in the state of the open dialog and in the one request that
 * sends it. Nothing here writes a value to storage, the URL, react-query or the console;
 * the dialog is unmounted on success, which takes every password input with it.
 *
 * The dialogs are native `<dialog>` elements opened with `showModal()`: focus trapping,
 * Escape and the inert background are the browser's job, not ours. Focus returns to the
 * button that opened the dialog when it closes.
 */

const NAME_RE = /^[a-z][a-z0-9-]{0,39}$/
const NAME_RULE = '1 to 40 lower-case letters, digits and dashes, starting with a letter.'
const SETTING_RE = /^[A-Za-z][A-Za-z0-9._-]{0,62}$/
const SECRET_KEY_RE = /^[A-Za-z][A-Za-z0-9_]{0,62}$/
const GROUP_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/
/** The words the backend refuses in a setting name: a secret belongs under Secret fields. */
const SECRET_WORD_RE = /password|secret|token|passphrase|credential/i
const SECRET_WORD_SENTENCE = 'That name holds a secret: put it under Secret fields'
const MAX_SETTINGS = 32
const MAX_SECRET_FIELDS = 16

/** `1m` to `24h`, the same rule the backend applies. */
function refreshOk(value: string): boolean {
  const match = /^(\d{1,4})([mh])$/.exec(value.trim())
  if (!match) return false
  const minutes = Number(match[1]) * (match[2] === 'h' ? 60 : 1)
  return minutes >= 1 && minutes <= 1440
}

/** The route for one connection; `secret` appends the rotation suffix. */
function connectionPath(connection: Connection, secret = false): string {
  const name = encodeURIComponent(connection.name)
  const base = connection.scope === 'personal'
    ? `/api/connections/personal/${name}`
    : connection.scope === 'group'
      ? `/api/connections/group/${encodeURIComponent(connection.ownerGroup ?? '')}/${name}`
      : `/api/connections/managed/${name}`
  return secret ? `${base}/secret` : base
}

type Row = { name: string; value: string }

/** The frame every dialog shares: heading, scrollable body, footer that stays visible. */
function DialogFrame({
  title,
  titleId,
  initialFocus,
  onClose,
  children,
  footer,
}: {
  title: string
  titleId: string
  /** A CSS selector inside the dialog; the first match gets focus on open. */
  initialFocus: string
  onClose: () => void
  children: ReactNode
  footer: ReactNode
}) {
  const ref = useRef<HTMLDialogElement>(null)
  const opener = useRef<HTMLElement | null>(null)

  useEffect(() => {
    const dialog = ref.current
    if (!dialog) return
    opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null
    if (!dialog.open) dialog.showModal()
    const target = dialog.querySelector<HTMLElement>(initialFocus)
    target?.focus()
    return () => {
      if (dialog.open) dialog.close()
      opener.current?.focus()
    }
  }, [initialFocus])

  return (
    <dialog
      ref={ref}
      aria-labelledby={titleId}
      onCancel={(event) => { event.preventDefault(); onClose() }}
      className="m-auto w-[min(44rem,calc(100vw_-_2rem))] max-w-none rounded-[14px] border border-border bg-bg-elev p-0 text-text shadow-2xl backdrop:bg-black/60"
    >
      <div className="flex max-h-[85vh] flex-col">
        <div className="px-5 pt-5">
          <h2 id={titleId} className="text-lg font-semibold break-words">{title}</h2>
        </div>
        <div className="min-h-0 overflow-y-auto px-5 py-4">{children}</div>
        <div className="flex flex-wrap items-center justify-end gap-2 border-t border-border px-5 py-3">
          {footer}
        </div>
      </div>
    </dialog>
  )
}

/** One visible label and its control. Number fields keep the browser's numeric keyboard. */
function FieldInput({
  id,
  field,
  value,
  onChange,
  secret,
  autoFocus,
}: {
  id: string
  field: Field
  value: string
  onChange: (value: string) => void
  secret?: boolean
  autoFocus?: boolean
}) {
  if (secret && field.multiline) {
    return (
      <label htmlFor={id} className="block">
        <span className="label block mb-1.5">{field.label}</span>
        <textarea
          id={id}
          rows={4}
          className="input font-mono"
          autoComplete="off"
          spellCheck={false}
          placeholder={field.placeholder}
          data-autofocus={autoFocus ? '' : undefined}
          value={value}
          onChange={(event) => onChange(event.target.value)}
        />
      </label>
    )
  }
  return (
    <label htmlFor={id} className="block">
      <span className="label block mb-1.5">{field.label}</span>
      <input
        id={id}
        type={secret ? 'password' : field.number ? 'number' : 'text'}
        className="input"
        autoComplete={secret ? 'new-password' : undefined}
        spellCheck={secret ? false : undefined}
        inputMode={field.number ? 'numeric' : undefined}
        placeholder={field.placeholder}
        data-autofocus={autoFocus ? '' : undefined}
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    </label>
  )
}

function ScopeOption({
  name,
  value,
  label,
  hint,
  checked,
  autoFocus,
  onChange,
}: {
  name: string
  value: string
  label: string
  hint: string
  checked: boolean
  autoFocus?: boolean
  onChange: () => void
}) {
  const id = useId()
  return (
    <label htmlFor={id} className="flex items-start gap-2 text-sm">
      <input
        id={id}
        type="radio"
        name={name}
        value={value}
        className="mt-1"
        checked={checked}
        aria-label={label}
        data-autofocus={autoFocus ? '' : undefined}
        onChange={onChange}
      />
      <span>
        <span className="block">{label}</span>
        <span className="block text-xs text-text-mute">{hint}</span>
      </span>
    </label>
  )
}

/** The rows of the `generic` type: settings (name and value) and secret fields. */
function GenericRows({
  rows,
  onChange,
  secret,
}: {
  rows: Row[]
  onChange: (rows: Row[]) => void
  secret?: boolean
}) {
  const limit = secret ? MAX_SECRET_FIELDS : MAX_SETTINGS
  const update = (index: number, patch: Partial<Row>) =>
    onChange(rows.map((row, i) => (i === index ? { ...row, ...patch } : row)))
  const secretWord = !secret && rows.some((row) => row.name.trim() !== '' && SECRET_WORD_RE.test(row.name))
  return (
    <div className="space-y-3">
      {rows.map((row, index) => (
        <div key={index} className="flex flex-wrap items-end gap-2">
          <label htmlFor={`${secret ? 'secret-row' : 'setting-row'}-${index}-name`} className="block min-w-40 flex-1">
            <span className="label block mb-1.5">{secret ? 'Secret field name' : 'Setting name'}</span>
            <input
              id={`${secret ? 'secret-row' : 'setting-row'}-${index}-name`}
              className="input font-mono"
              value={row.name}
              spellCheck={false}
              autoComplete="off"
              onChange={(event) => update(index, { name: event.target.value })}
            />
          </label>
          <label htmlFor={`${secret ? 'secret-row' : 'setting-row'}-${index}-value`} className="block min-w-40 flex-1">
            <span className="label block mb-1.5">{secret ? 'Secret value' : 'Setting value'}</span>
            <input
              id={`${secret ? 'secret-row' : 'setting-row'}-${index}-value`}
              className="input"
              type={secret ? 'password' : 'text'}
              autoComplete={secret ? 'new-password' : 'off'}
              spellCheck={secret ? false : undefined}
              value={row.value}
              onChange={(event) => update(index, { value: event.target.value })}
            />
          </label>
          <button
            type="button"
            className="btn-ghost"
            aria-label={`Remove ${secret ? 'secret field' : 'setting'} ${index + 1}`}
            onClick={() => onChange(rows.filter((_, i) => i !== index))}
          >
            Remove
          </button>
        </div>
      ))}
      {secretWord && <p className="text-xs text-bad">{SECRET_WORD_SENTENCE}</p>}
      {rows.length < limit && (
        <button
          type="button"
          className="btn-ghost"
          onClick={() => onChange([...rows, { name: '', value: '' }])}
        >
          Add {secret ? 'secret field' : 'setting'}
        </button>
      )}
    </div>
  )
}

// ---------------------------------------------------------------------------
// New connection
// ---------------------------------------------------------------------------

export function NewConnectionDialog({
  me,
  onClose,
  onCreated,
}: {
  me?: Me
  onClose: () => void
  onCreated: (name: string) => void
}) {
  const prefix = useId()
  const titleId = `${prefix}-title`
  const formId = `${prefix}-form`
  const groups = me?.groups ?? []
  const admin = isAdmin(me)

  const [scope, setScope] = useState<Connection['scope']>('personal')
  const [ownerGroup, setOwnerGroup] = useState(groups[0] ?? '')
  const [usableBy, setUsableBy] = useState<string[]>([])
  const [usableInput, setUsableInput] = useState('')
  const [usableError, setUsableError] = useState<string | null>(null)
  const [type, setType] = useState('postgresql')
  const [name, setName] = useState('')
  const [nameTouched, setNameTouched] = useState(false)
  const [config, setConfig] = useState<Record<string, string>>({})
  const [secrets, setSecrets] = useState<Record<string, string>>({})
  const [variantId, setVariantId] = useState(CONNECTION_TYPES.postgresql.variants[0].id)
  const [refresh, setRefresh] = useState('15m')
  const [refreshTouched, setRefreshTouched] = useState(false)
  const [genericConfig, setGenericConfig] = useState<Row[]>([{ name: '', value: '' }])
  const [genericSecrets, setGenericSecrets] = useState<Row[]>([{ name: '', value: '' }])
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const spec = CONNECTION_TYPES[type]
  const variant = spec?.variants.find((v) => v.id === variantId) ?? spec?.variants[0]
  const generic = type === 'generic'

  // The group options can arrive after the dialog opened (the `/api/me` read is async).
  useEffect(() => {
    if (scope === 'group' && !ownerGroup && groups.length > 0) setOwnerGroup(groups[0])
  }, [scope, ownerGroup, groups])

  function changeType(next: string) {
    const nextSpec = CONNECTION_TYPES[next]
    setType(next)
    setConfig({})
    setSecrets({})
    setVariantId(nextSpec?.variants[0]?.id ?? '')
    setGenericConfig([{ name: '', value: '' }])
    setGenericSecrets([{ name: '', value: '' }])
    setError(null)
  }

  function addGroup() {
    const group = usableInput.trim()
    if (!group) return
    if (!GROUP_RE.test(group)) {
      setUsableError('Use a group name: letters, digits, dots, dashes and underscores, starting with a letter or digit.')
      return
    }
    if (usableBy.includes(group)) {
      setUsableError('That group is already on the list.')
      return
    }
    if (usableBy.length >= 20) {
      setUsableError('At most 20 groups.')
      return
    }
    setUsableBy([...usableBy, group])
    setUsableInput('')
    setUsableError(null)
  }

  const genericIncomplete = generic && (
    !genericSecrets.some((row) => row.name.trim() !== '' && row.value !== '')
    || genericSecrets.some((row) => {
      const filled = row.name.trim() !== '' || row.value !== ''
      return filled && (row.name.trim() === '' || row.value === '' || !SECRET_KEY_RE.test(row.name.trim()))
    })
    || genericConfig.some((row) => {
      const filled = row.name.trim() !== '' || row.value !== ''
      return filled && (row.name.trim() === '' || row.value === '' || !SETTING_RE.test(row.name.trim()))
    })
    || genericConfig.some((row) => row.name.trim() !== '' && SECRET_WORD_RE.test(row.name))
  )

  const fixedIncomplete = !generic && (
    !spec
    || !variant
    || spec.config.some((field) => field.required && !config[field.name]?.trim())
    || variant.secret.some((field) => field.required && !secrets[field.name])
  )

  const disabled = submitting
    || !NAME_RE.test(name)
    || (scope === 'group' && groups.length === 0)
    || (scope === 'group' && !ownerGroup)
    || (scope === 'managed' && usableBy.length === 0)
    || (scope !== 'personal' && !refreshOk(refresh))
    || genericIncomplete
    || fixedIncomplete

  async function submit(event: FormEvent) {
    event.preventDefault()
    if (disabled) return
    setSubmitting(true)
    setError(null)

    const configPayload: Record<string, string> = {}
    if (generic) {
      for (const row of genericConfig) {
        if (row.name.trim() !== '' && row.value !== '') configPayload[row.name.trim()] = row.value
      }
    } else if (spec) {
      for (const field of spec.config) {
        const value = config[field.name]?.trim() ?? ''
        if (value !== '') configPayload[field.name] = value
      }
    }

    const secretPayload: Record<string, string> = {}
    if (generic) {
      for (const row of genericSecrets) {
        if (row.name.trim() !== '' && row.value !== '') secretPayload[row.name.trim()] = row.value
      }
    } else if (variant) {
      for (const field of variant.secret) {
        const value = secrets[field.name] ?? ''
        if (value !== '') secretPayload[field.name] = value
      }
    }

    try {
      await api.createConnection({
        name,
        type,
        scope,
        ...(scope === 'group' ? { ownerGroup } : {}),
        ...(scope === 'managed' ? { usableBy } : {}),
        config: configPayload,
        secret: secretPayload,
        ...(scope !== 'personal' ? { refreshInterval: refresh.trim() } : {}),
      })
      onCreated(name)
    } catch (e: unknown) {
      setError(errorMessage(e))
      setSubmitting(false)
    }
  }

  return (
    <DialogFrame
      title="New connection"
      titleId={titleId}
      initialFocus="[data-autofocus]"
      onClose={onClose}
      footer={
        <>
          <button type="submit" form={formId} className="btn-primary" disabled={disabled}>
            {submitting ? 'Creating…' : 'Create connection'}
          </button>
          <button type="button" className="btn-ghost" onClick={onClose}>Cancel</button>
        </>
      }
    >
      <form id={formId} className="space-y-5" onSubmit={(event) => void submit(event)}>
        <fieldset>
          <legend className="label mb-2">Scope</legend>
          <div className="space-y-2">
            <ScopeOption
              name={`${prefix}-scope`}
              value="personal"
              label="Personal"
              hint="Only you"
              checked={scope === 'personal'}
              autoFocus
              onChange={() => setScope('personal')}
            />
            <ScopeOption
              name={`${prefix}-scope`}
              value="group"
              label="Group"
              hint="Everyone in a group you belong to"
              checked={scope === 'group'}
              onChange={() => setScope('group')}
            />
            {admin && (
              <ScopeOption
                name={`${prefix}-scope`}
                value="managed"
                label="Managed"
                hint="Platform services only. Administrators only."
                checked={scope === 'managed'}
                onChange={() => setScope('managed')}
              />
            )}
          </div>
        </fieldset>

        {scope === 'group' && (
          <div className="space-y-2">
            {groups.length === 0 ? (
              <p className="text-sm text-bad">You are not a member of any group.</p>
            ) : (
              <label htmlFor={`${prefix}-owner`} className="block">
                <span className="label block mb-1.5">Owner group</span>
                <select
                  id={`${prefix}-owner`}
                  className="input"
                  value={ownerGroup}
                  onChange={(event) => setOwnerGroup(event.target.value)}
                >
                  {groups.map((group) => <option key={group} value={group}>{group}</option>)}
                </select>
              </label>
            )}
          </div>
        )}

        {scope === 'managed' && (
          <div className="space-y-2">
            <label htmlFor={`${prefix}-usable`} className="block">
              <span className="label block mb-1.5">Used by</span>
              <div className="flex flex-wrap gap-2">
                <input
                  id={`${prefix}-usable`}
                  className="input min-w-40 flex-1"
                  value={usableInput}
                  placeholder="Group name"
                  autoComplete="off"
                  spellCheck={false}
                  onChange={(event) => { setUsableInput(event.target.value); setUsableError(null) }}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') {
                      event.preventDefault()
                      addGroup()
                    }
                  }}
                />
                <button type="button" className="btn-ghost" onClick={addGroup}>Add group</button>
              </div>
            </label>
            <p className="text-xs text-text-mute">Type a group name and press Enter. At least one.</p>
            {usableBy.length > 0 && (
              <ul className="flex flex-wrap gap-2" aria-label="Groups that may use the connection">
                {usableBy.map((group) => (
                  <li key={group} className="badge border-border bg-bg-soft text-text-dim">
                    <span className="font-mono">{group}</span>
                    <button
                      type="button"
                      aria-label={`Remove ${group}`}
                      onClick={() => setUsableBy(usableBy.filter((g) => g !== group))}
                      className="ml-1"
                    >
                      ×
                    </button>
                  </li>
                ))}
              </ul>
            )}
            {usableError && <p className="text-xs text-bad" role="alert">{usableError}</p>}
          </div>
        )}

        <div className="grid gap-4 sm:grid-cols-2">
          <label htmlFor={`${prefix}-type`} className="block">
            <span className="label block mb-1.5">Type</span>
            <select
              id={`${prefix}-type`}
              className="input"
              value={type}
              onChange={(event) => changeType(event.target.value)}
            >
              {Object.entries(CONNECTION_TYPES).map(([id, typeSpec]) => (
                <option key={id} value={id}>{typeSpec.label}</option>
              ))}
            </select>
          </label>

          <div>
            <label htmlFor={`${prefix}-name`} className="block">
              <span className="label block mb-1.5">Name</span>
              <input
                id={`${prefix}-name`}
                className="input font-mono"
                value={name}
                autoComplete="off"
                spellCheck={false}
                aria-describedby={`${prefix}-name-hint`}
                onChange={(event) => setName(event.target.value)}
                onBlur={() => setNameTouched(true)}
              />
            </label>
            <p id={`${prefix}-name-hint`} className="mt-1 text-xs text-text-mute">{NAME_RULE}</p>
            {nameTouched && !NAME_RE.test(name) && (
              <p className="mt-1 text-xs text-bad">Use {NAME_RULE}</p>
            )}
          </div>
        </div>

        {generic ? (
          <>
            <fieldset>
              <legend className="label mb-2">Settings</legend>
              <GenericRows rows={genericConfig} onChange={setGenericConfig} />
            </fieldset>
            <fieldset>
              <legend className="label mb-2">Secret fields</legend>
              <GenericRows rows={genericSecrets} onChange={setGenericSecrets} secret />
            </fieldset>
          </>
        ) : (
          <>
            {spec && spec.config.length > 0 && (
              <fieldset className="space-y-3">
                <legend className="label mb-2">Settings</legend>
                {spec.config.map((field) => (
                  <FieldInput
                    key={field.name}
                    id={`${prefix}-config-${field.name}`}
                    field={field}
                    value={config[field.name] ?? ''}
                    onChange={(value) => setConfig((current) => ({ ...current, [field.name]: value }))}
                  />
                ))}
              </fieldset>
            )}

            {spec && spec.variants.length > 1 && (
              <fieldset>
                <legend className="label mb-2">Credentials</legend>
                <div className="flex flex-wrap gap-x-5 gap-y-2">
                  {spec.variants.map((option) => (
                    <label key={option.id} className="flex items-center gap-2 text-sm">
                      <input
                        type="radio"
                        name={`${prefix}-variant`}
                        value={option.id}
                        checked={variant?.id === option.id}
                        onChange={() => { setVariantId(option.id); setSecrets({}) }}
                      />
                      {option.label}
                    </label>
                  ))}
                </div>
              </fieldset>
            )}

            {variant && (
              <fieldset className="space-y-3">
                <legend className="label mb-2">Secret fields</legend>
                {variant.secret.map((field) => (
                  <FieldInput
                    key={field.name}
                    id={`${prefix}-secret-${field.name}`}
                    field={field}
                    value={secrets[field.name] ?? ''}
                    secret
                    onChange={(value) => setSecrets((current) => ({ ...current, [field.name]: value }))}
                  />
                ))}
              </fieldset>
            )}
          </>
        )}

        {scope !== 'personal' && (
          <details className="rounded-[10px] border border-border/70 px-3 py-2">
            <summary className="cursor-pointer text-sm">Advanced</summary>
            <div className="mt-3">
              <label htmlFor={`${prefix}-refresh`} className="block">
                <span className="label block mb-1.5">Refresh interval</span>
                <input
                  id={`${prefix}-refresh`}
                  className="input"
                  value={refresh}
                  autoComplete="off"
                  spellCheck={false}
                  aria-describedby={`${prefix}-refresh-hint`}
                  onChange={(event) => setRefresh(event.target.value)}
                  onBlur={() => setRefreshTouched(true)}
                />
              </label>
              <p id={`${prefix}-refresh-hint`} className="mt-1 text-xs text-text-mute">1m to 24h</p>
              {refreshTouched && !refreshOk(refresh) && (
                <p className="mt-1 text-xs text-bad">Use a number and m or h, for example 15m.</p>
              )}
            </div>
          </details>
        )}

        {error && (
          <div role="alert" className="rounded-[10px] border border-red-500/40 bg-red-500/10 p-3 text-sm text-bad break-words">
            {error}
          </div>
        )}
      </form>
    </DialogFrame>
  )
}

// ---------------------------------------------------------------------------
// Rotate
// ---------------------------------------------------------------------------

export function RotateConnectionDialog({
  connection,
  onClose,
  onRotated,
}: {
  connection: Connection
  onClose: () => void
  onRotated: (name: string) => void
}) {
  const prefix = useId()
  const titleId = `${prefix}-title`
  const formId = `${prefix}-form`
  const keys = connection.secretKeys
  const managed = connection.scope === 'managed'
  const [secret, setSecret] = useState<Record<string, string>>(() =>
    Object.fromEntries(keys.map((key) => [key, ''])),
  )
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const filled = keys.filter((key) => (secret[key] ?? '') !== '')
  const disabled = submitting || keys.length === 0 || (managed ? filled.length !== keys.length : filled.length === 0)

  async function submit(event: FormEvent) {
    event.preventDefault()
    if (disabled) return
    setSubmitting(true)
    setError(null)
    const patch: Record<string, string> = {}
    for (const key of filled) patch[key] = secret[key]
    try {
      await api.rotateConnection(connectionPath(connection, true), patch)
      onRotated(connection.name)
    } catch (e: unknown) {
      setError(errorMessage(e))
      setSubmitting(false)
    }
  }

  return (
    <DialogFrame
      title={`Rotate ${connection.name}`}
      titleId={titleId}
      initialFocus="[data-autofocus]"
      onClose={onClose}
      footer={
        <>
          <button type="submit" form={formId} className="btn-primary" disabled={disabled}>
            {submitting ? 'Rotating…' : 'Rotate connection'}
          </button>
          <button type="button" className="btn-ghost" onClick={onClose} data-autofocus={keys.length === 0 ? '' : undefined}>
            Cancel
          </button>
        </>
      }
    >
      <form id={formId} className="space-y-4" onSubmit={(event) => void submit(event)}>
        {managed && (
          <p className="text-sm text-text-dim">A managed connection cannot be read back, so enter every field.</p>
        )}
        {keys.length === 0 ? (
          <p className="text-sm text-text-mute">This connection has no secret fields to rotate.</p>
        ) : (
          <>
            {!managed && <p className="text-sm text-text-mute">Enter the new value of at least one field.</p>}
            {keys.map((key, index) => (
              <label key={key} htmlFor={`${prefix}-${key}`} className="block">
                <span className="block mb-1.5 text-xs font-mono text-text-mute">{key}</span>
                <input
                  id={`${prefix}-${key}`}
                  type="password"
                  className="input"
                  autoComplete="new-password"
                  spellCheck={false}
                  data-autofocus={index === 0 ? '' : undefined}
                  value={secret[key] ?? ''}
                  onChange={(event) => setSecret((current) => ({ ...current, [key]: event.target.value }))}
                />
              </label>
            ))}
          </>
        )}
        {error && (
          <div role="alert" className="rounded-[10px] border border-red-500/40 bg-red-500/10 p-3 text-sm text-bad break-words">
            {error}
          </div>
        )}
      </form>
    </DialogFrame>
  )
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

export function DeleteConnectionDialog({
  connection,
  onClose,
  onDeleted,
}: {
  connection: Connection
  onClose: () => void
  onDeleted: (name: string) => void
}) {
  const prefix = useId()
  const titleId = `${prefix}-title`
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const consequence = connection.scope === 'personal'
    ? 'Notebooks that use it will fail.'
    : 'Notebooks that use it will fail. Pipelines and catalogs that use it will stop working.'

  async function remove() {
    if (submitting) return
    setSubmitting(true)
    setError(null)
    try {
      await api.deleteConnection(connectionPath(connection))
      onDeleted(connection.name)
    } catch (e: unknown) {
      setError(errorMessage(e))
      setSubmitting(false)
    }
  }

  return (
    <DialogFrame
      title={`Delete ${connection.name}?`}
      titleId={titleId}
      initialFocus="[data-autofocus]"
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn-danger" disabled={submitting} onClick={() => void remove()}>
            {submitting ? 'Deleting…' : 'Delete connection'}
          </button>
          <button type="button" className="btn-ghost" onClick={onClose} data-autofocus>Cancel</button>
        </>
      }
    >
      <p className="text-sm text-text-dim">{consequence}</p>
      {error && (
        <div role="alert" className="mt-3 rounded-[10px] border border-red-500/40 bg-red-500/10 p-3 text-sm text-bad break-words">
          {error}
        </div>
      )}
    </DialogFrame>
  )
}
