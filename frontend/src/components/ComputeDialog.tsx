import { useEffect, useId, useRef, useState, type FormEvent } from 'react'
import { api, errorMessage, type ComputeCluster, type ComputeSpecBody } from '../api'

/** The same rules the API enforces (`backend/src/compute.rs`), so a refusal is rare. */
const NAME_RE = /^[a-z][a-z0-9-]{0,39}$/
const GROUP_RE = /^[A-Za-z0-9][A-Za-z0-9._-]*$/
const USER_RE = /^[A-Za-z0-9][A-Za-z0-9._@-]*$/
const RUNTIME_RE = /^spark-[0-9]+\.[0-9]+-py3\.[0-9]+$/
const MEMORY_RE = /^[1-9][0-9]*(Mi|Gi)$/
const ENV_NAME_RE = /^[A-Z_][A-Z0-9_]*$/

/** Settings the platform owns; the operator refuses them, so the dialog blocks them. */
function MANAGED(key: string): boolean {
  return (
    key.startsWith('spark.kubernetes.authenticate.') ||
    key === 'spark.kubernetes.namespace' ||
    key.includes('serviceAccount') ||
    (key.startsWith('spark.hadoop.fs.s3a.') && key.includes('key')) ||
    (key.startsWith('spark.sql.catalog.') && (key.includes('credential') || key.includes('token')))
  )
}

const NAME_SENTENCE =
  'use lower-case letters, digits and dashes, e.g. mobility (a letter first, at most 40, no dash at the end)'
const GROUP_SENTENCE = 'a group of your sign-in provider, e.g. mobility'
const USER_SENTENCE = 'a user name: letters, digits, dots, dashes, underscores or @'
const RUNTIME_SENTENCE = 'like spark-4.1-py3.11'
const MEMORY_SENTENCE = 'like 512Mi or 4Gi'
const ENV_SENTENCE = 'upper-case letters, digits and underscores, starting with a letter or underscore'
const CORES_SENTENCE = '1 to 16 cores'
const EXECUTORS_SENTENCE = '0 to 50 executors'
const IDLE_SENTENCE = '0 to 1440 minutes; 0 = never, only for the shared cluster'
const IMAGE_SENTENCE = 'must be pinned by @sha256: digest'

type Row = { name: string; value: string }

function numberOrNull(text: string): number | null {
  const trimmed = text.trim()
  return trimmed === '' ? null : Number(trimmed)
}

function wholeNumber(text: string, min: number, max: number): boolean {
  if (text.trim() === '') return true
  const value = Number(text.trim())
  return Number.isInteger(value) && value >= min && value <= max
}

/**
 * The New and Edit dialogs of the Compute page (internal#161 slice 5d): a native
 * `<dialog>` like `NewTeamDialog`, with the API's own rules checked before the call.
 * `cluster === null` creates one; otherwise the form edits the named cluster.
 */
export default function ComputeDialog({
  cluster,
  onClose,
  onSaved,
  onDeleted,
}: {
  cluster: ComputeCluster | null
  onClose: () => void
  onSaved: (c: ComputeCluster) => void
  onDeleted: () => void
}) {
  const titleId = useId()
  const nameId = useId()
  const displayNameId = useId()
  const ownerId = useId()
  const ownerKindId = useId()
  const sizeId = useId()
  const driverCoresId = useId()
  const driverMemoryId = useId()
  const executorCoresId = useId()
  const executorMemoryId = useId()
  const maxExecutorsId = useId()
  const runtimeId = useId()
  const imageId = useId()
  const idleId = useId()
  const envId = useId()
  const confId = useId()

  const dialog = useRef<HTMLDialogElement>(null)
  const opener = useRef<HTMLElement | null>(null)

  const editing = cluster !== null
  const [name, setName] = useState(cluster?.name ?? '')
  const [displayName, setDisplayName] = useState(cluster?.displayName ?? '')
  const [ownerKind, setOwnerKind] = useState<'group' | 'user'>(
    cluster?.owner.user && !cluster.owner.group ? 'user' : 'group',
  )
  const [ownerValue, setOwnerValue] = useState(
    (cluster?.owner.group ?? cluster?.owner.user ?? '') || '',
  )
  const [size, setSize] = useState<'small' | 'medium' | 'large'>(
    cluster?.size === 'medium' || cluster?.size === 'large' ? cluster.size : 'small',
  )
  const [driverCores, setDriverCores] = useState(
    cluster?.driver?.cores != null ? String(cluster.driver.cores) : '',
  )
  const [driverMemory, setDriverMemory] = useState(cluster?.driver?.memory ?? '')
  const [executorCores, setExecutorCores] = useState(
    cluster?.executor?.cores != null ? String(cluster.executor.cores) : '',
  )
  const [executorMemory, setExecutorMemory] = useState(cluster?.executor?.memory ?? '')
  const [maxExecutors, setMaxExecutors] = useState(
    cluster?.maxExecutors != null ? String(cluster.maxExecutors) : '',
  )
  const [runtime, setRuntime] = useState(cluster?.runtime ?? 'spark-4.1-py3.11')
  const [image, setImage] = useState(cluster?.image ?? '')
  const [idle, setIdle] = useState(String(cluster?.idleStopMinutes ?? 60))
  const [env, setEnv] = useState<Row[]>(() => (cluster?.env ?? []).map((v) => ({ ...v })))
  const [conf, setConf] = useState<Row[]>(() =>
    Object.entries(cluster?.sparkConf ?? {}).map(([key, value]) => ({ name: key, value })),
  )
  const [saving, setSaving] = useState(false)
  const [deleting, setDeleting] = useState(false)
  const [confirmDelete, setConfirmDelete] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const element = dialog.current
    if (!element) return
    opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null
    if (!element.open) element.showModal()
    return () => {
      if (element.open) element.close()
      opener.current?.focus()
    }
  }, [])

  const nameValue = name.trim()
  const ownerValueTrimmed = ownerValue.trim()
  const runtimeValue = runtime.trim()
  const imageValue = image.trim()

  const nameProblem = editing
    ? null
    : nameValue === ''
      ? 'a name is required'
      : !NAME_RE.test(nameValue) || nameValue.endsWith('-')
        ? NAME_SENTENCE
        : null
  const ownerProblem =
    ownerValueTrimmed === ''
      ? 'who may use this cluster is required'
      : !(ownerKind === 'group' ? GROUP_RE : USER_RE).test(ownerValueTrimmed)
        ? ownerKind === 'group'
          ? GROUP_SENTENCE
          : USER_SENTENCE
        : null
  const runtimeProblem =
    runtimeValue === ''
      ? 'a runtime is required'
      : !RUNTIME_RE.test(runtimeValue)
        ? RUNTIME_SENTENCE
        : null
  const imageProblem = imageValue !== '' && !imageValue.includes('@sha256:') ? IMAGE_SENTENCE : null

  const driverHalfProblem =
    (driverCores.trim() === '') !== (driverMemory.trim() === '') ? 'set both or neither' : null
  const executorHalfProblem =
    (executorCores.trim() === '') !== (executorMemory.trim() === '') ? 'set both or neither' : null
  const driverCoresProblem = wholeNumber(driverCores, 1, 16) ? null : CORES_SENTENCE
  const executorCoresProblem = wholeNumber(executorCores, 1, 16) ? null : CORES_SENTENCE
  const driverMemoryProblem =
    driverMemory.trim() === '' || MEMORY_RE.test(driverMemory.trim()) ? null : MEMORY_SENTENCE
  const executorMemoryProblem =
    executorMemory.trim() === '' || MEMORY_RE.test(executorMemory.trim()) ? null : MEMORY_SENTENCE
  const maxExecutorsProblem = wholeNumber(maxExecutors, 0, 50) ? null : EXECUTORS_SENTENCE

  const idleNumber = Number(idle.trim())
  const effectiveName = editing ? cluster.name : nameValue
  const idleProblem =
    idle.trim() === '' || !Number.isInteger(idleNumber) || idleNumber < 0 || idleNumber > 1440
      ? IDLE_SENTENCE
      : idleNumber === 0 && effectiveName !== 'shared'
        ? '0 = never is only for the shared cluster'
        : null

  const envProblems = env.map((row, index) => {
    const key = row.name.trim()
    if (key === '' && row.value === '') return null
    if (key === '') return 'a name is required'
    if (!ENV_NAME_RE.test(key)) return ENV_SENTENCE
    if (env.some((other, i) => i !== index && other.name.trim() === key)) return 'duplicate name'
    return null
  })
  const confProblems = conf.map((row, index) => {
    const key = row.name.trim()
    if (key === '' && row.value === '') return null
    if (key === '') return 'a key is required'
    if (conf.some((other, i) => i !== index && other.name.trim() === key)) return 'duplicate key'
    if (MANAGED(key)) return 'managed by the platform'
    return null
  })

  const problems = [
    nameProblem,
    ownerProblem,
    runtimeProblem,
    imageProblem,
    driverHalfProblem,
    executorHalfProblem,
    driverCoresProblem,
    driverMemoryProblem,
    executorCoresProblem,
    executorMemoryProblem,
    maxExecutorsProblem,
    idleProblem,
    ...envProblems,
    ...confProblems,
  ]
  const canSave = !saving && !deleting && problems.every((problem) => problem === null)

  function resources(cores: string, memory: string): { cores: number; memory: string } | null {
    if (cores.trim() === '' && memory.trim() === '') return null
    return { cores: Number(cores.trim()), memory: memory.trim() }
  }

  function body(): ComputeSpecBody {
    const driver = resources(driverCores, driverMemory)
    const executor = resources(executorCores, executorMemory)
    const max = numberOrNull(maxExecutors)
    return {
      ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
      owner:
        ownerKind === 'group'
          ? { group: ownerValueTrimmed, user: null }
          : { group: null, user: ownerValueTrimmed },
      size,
      ...(driver ? { driver } : {}),
      ...(executor ? { executor } : {}),
      ...(max !== null ? { maxExecutors: max } : {}),
      runtime: runtimeValue,
      ...(imageValue ? { image: imageValue } : {}),
      env: env
        .filter((row) => row.name.trim() !== '')
        .map((row) => ({ name: row.name.trim(), value: row.value })),
      sparkConf: Object.fromEntries(
        conf.filter((row) => row.name.trim() !== '').map((row) => [row.name.trim(), row.value]),
      ),
      idleStopMinutes: idleNumber,
    }
  }

  async function save(event: FormEvent) {
    event.preventDefault()
    if (!canSave) return
    const clusterName = editing ? cluster.name : nameValue
    setSaving(true)
    setError(null)
    try {
      const saved = await api.saveCompute(clusterName, body())
      onSaved(saved)
    } catch (e) {
      setSaving(false)
      setError(errorMessage(e))
    }
  }

  async function remove() {
    if (!editing || cluster.shared) return
    if (!confirmDelete) {
      setConfirmDelete(true)
      return
    }
    setDeleting(true)
    setError(null)
    try {
      await api.deleteCompute(cluster.name)
      onDeleted()
    } catch (e) {
      setDeleting(false)
      setError(errorMessage(e))
    }
  }

  const problemClass = 'mt-1 text-xs text-bad'
  const hintClass = 'mt-1 text-xs text-text-mute'

  return (
    <dialog
      ref={dialog}
      aria-labelledby={titleId}
      onCancel={(event) => {
        event.preventDefault()
        onClose()
      }}
      className="m-auto w-[min(46rem,calc(100vw_-_2rem))] max-w-none rounded-[14px] border border-border bg-bg-elev p-0 text-text shadow-2xl backdrop:bg-black/60"
    >
      <form onSubmit={save} className="flex max-h-[85vh] flex-col">
        <div className="px-5 pt-5">
          <h2 id={titleId} className="text-lg font-semibold break-words">
            {editing ? `Edit ${cluster.name}` : 'New cluster'}
          </h2>
          <p className="mt-1 text-xs text-text-mute">
            A Spark cluster is a <span className="font-mono">SparkCluster</span> resource; its pods are
            created when it starts and removed when it stops.
          </p>
        </div>

        <div className="min-h-0 space-y-4 overflow-y-auto px-5 py-4">
          <div>
            <label htmlFor={nameId} className="label block mb-1.5">Name</label>
            <input
              id={nameId}
              className="input w-full"
              value={name}
              disabled={editing}
              autoComplete="off"
              spellCheck={false}
              aria-invalid={nameProblem ? true : undefined}
              aria-describedby={`${nameId}-hint`}
              onChange={(event) => setName(event.target.value)}
            />
            <p id={`${nameId}-hint`} className={nameProblem ? problemClass : hintClass}>
              {nameProblem ?? 'lower-case letters, digits and dashes, e.g. mobility'}
            </p>
          </div>

          <div>
            <label htmlFor={displayNameId} className="label block mb-1.5">
              Display name <span className="text-text-mute">(optional)</span>
            </label>
            <input
              id={displayNameId}
              className="input w-full"
              value={displayName}
              autoComplete="off"
              onChange={(event) => setDisplayName(event.target.value)}
            />
            <p className={hintClass}>shown on the card instead of the name</p>
          </div>

          <div>
            <label htmlFor={ownerKindId} className="label block mb-1.5">Owner</label>
            <div className="flex gap-2">
              <select
                id={ownerKindId}
                className="input"
                value={ownerKind}
                onChange={(event) => setOwnerKind(event.target.value === 'user' ? 'user' : 'group')}
              >
                <option value="group">group</option>
                <option value="user">user</option>
              </select>
              <input
                id={ownerId}
                className="input flex-1"
                value={ownerValue}
                autoComplete="off"
                spellCheck={false}
                aria-invalid={ownerProblem ? true : undefined}
                aria-describedby={`${ownerId}-hint`}
                onChange={(event) => setOwnerValue(event.target.value)}
              />
            </div>
            <p id={`${ownerId}-hint`} className={ownerProblem ? problemClass : hintClass}>
              {ownerProblem ??
                (ownerKind === 'group'
                  ? 'who may use this cluster: a group of your sign-in provider'
                  : 'who may use this cluster: one person')}
            </p>
          </div>

          <div>
            <label htmlFor={sizeId} className="label block mb-1.5">Size</label>
            <select
              id={sizeId}
              className="input"
              value={size}
              onChange={(event) =>
                setSize(event.target.value === 'large' ? 'large' : event.target.value === 'medium' ? 'medium' : 'small')
              }
            >
              <option value="small">small</option>
              <option value="medium">medium</option>
              <option value="large">large</option>
            </select>
            <p className={hintClass}>the preset of driver, executor and executor count</p>
          </div>

          <fieldset>
            <legend className="label mb-1.5">Driver</legend>
            <div className="flex gap-2">
              <div className="flex-1">
                <label htmlFor={driverCoresId} className="mb-1 block text-xs text-text-dim">cores</label>
                <input
                  id={driverCoresId}
                  className="input w-full"
                  type="number"
                  min={1}
                  max={16}
                  value={driverCores}
                  aria-invalid={driverCoresProblem || driverHalfProblem ? true : undefined}
                  aria-describedby={`${driverCoresId}-hint`}
                  onChange={(event) => setDriverCores(event.target.value)}
                />
                <p id={`${driverCoresId}-hint`} className={driverCoresProblem ? problemClass : hintClass}>
                  {driverCoresProblem ?? driverHalfProblem ?? '1 to 16; blank means the size'}
                </p>
              </div>
              <div className="flex-1">
                <label htmlFor={driverMemoryId} className="mb-1 block text-xs text-text-dim">memory</label>
                <input
                  id={driverMemoryId}
                  className="input w-full"
                  value={driverMemory}
                  placeholder="2Gi"
                  autoComplete="off"
                  spellCheck={false}
                  aria-invalid={driverMemoryProblem || driverHalfProblem ? true : undefined}
                  aria-describedby={`${driverMemoryId}-hint`}
                  onChange={(event) => setDriverMemory(event.target.value)}
                />
                <p id={`${driverMemoryId}-hint`} className={driverMemoryProblem ? problemClass : hintClass}>
                  {driverMemoryProblem ?? driverHalfProblem ?? 'like 2Gi; blank means the size'}
                </p>
              </div>
            </div>
          </fieldset>

          <fieldset>
            <legend className="label mb-1.5">Executor</legend>
            <div className="flex gap-2">
              <div className="flex-1">
                <label htmlFor={executorCoresId} className="mb-1 block text-xs text-text-dim">cores</label>
                <input
                  id={executorCoresId}
                  className="input w-full"
                  type="number"
                  min={1}
                  max={16}
                  value={executorCores}
                  aria-invalid={executorCoresProblem || executorHalfProblem ? true : undefined}
                  aria-describedby={`${executorCoresId}-hint`}
                  onChange={(event) => setExecutorCores(event.target.value)}
                />
                <p id={`${executorCoresId}-hint`} className={executorCoresProblem ? problemClass : hintClass}>
                  {executorCoresProblem ?? executorHalfProblem ?? '1 to 16; blank means the size'}
                </p>
              </div>
              <div className="flex-1">
                <label htmlFor={executorMemoryId} className="mb-1 block text-xs text-text-dim">memory</label>
                <input
                  id={executorMemoryId}
                  className="input w-full"
                  value={executorMemory}
                  placeholder="2Gi"
                  autoComplete="off"
                  spellCheck={false}
                  aria-invalid={executorMemoryProblem || executorHalfProblem ? true : undefined}
                  aria-describedby={`${executorMemoryId}-hint`}
                  onChange={(event) => setExecutorMemory(event.target.value)}
                />
                <p id={`${executorMemoryId}-hint`} className={executorMemoryProblem ? problemClass : hintClass}>
                  {executorMemoryProblem ?? executorHalfProblem ?? 'like 2Gi; blank means the size'}
                </p>
              </div>
            </div>
          </fieldset>

          <div>
            <label htmlFor={maxExecutorsId} className="label block mb-1.5">
              Max executors <span className="text-text-mute">(optional)</span>
            </label>
            <input
              id={maxExecutorsId}
              className="input w-full"
              type="number"
              min={0}
              max={50}
              value={maxExecutors}
              aria-invalid={maxExecutorsProblem ? true : undefined}
              aria-describedby={`${maxExecutorsId}-hint`}
              onChange={(event) => setMaxExecutors(event.target.value)}
            />
            <p id={`${maxExecutorsId}-hint`} className={maxExecutorsProblem ? problemClass : hintClass}>
              {maxExecutorsProblem ?? '0 to 50; blank means the size preset'}
            </p>
          </div>

          <div>
            <label htmlFor={runtimeId} className="label block mb-1.5">Runtime</label>
            <input
              id={runtimeId}
              className="input w-full"
              value={runtime}
              autoComplete="off"
              spellCheck={false}
              aria-invalid={runtimeProblem ? true : undefined}
              aria-describedby={`${runtimeId}-hint`}
              onChange={(event) => setRuntime(event.target.value)}
            />
            <p id={`${runtimeId}-hint`} className={runtimeProblem ? problemClass : hintClass}>
              {runtimeProblem ?? 'spark-<version>-py3.<minor>; the platform\u2019s catalogue ships spark-4.1-py3.11'}
            </p>
          </div>

          <div>
            <label htmlFor={imageId} className="label block mb-1.5">
              Image <span className="text-text-mute">(optional)</span>
            </label>
            <input
              id={imageId}
              className="input w-full"
              value={image}
              autoComplete="off"
              spellCheck={false}
              aria-invalid={imageProblem ? true : undefined}
              aria-describedby={`${imageId}-hint`}
              onChange={(event) => setImage(event.target.value)}
            />
            <p id={`${imageId}-hint`} className={imageProblem ? problemClass : hintClass}>
              {imageProblem ?? 'an optional image built FROM a runtime image, pinned by @sha256: digest'}
            </p>
          </div>

          <div>
            <label htmlFor={idleId} className="label block mb-1.5">Idle minutes</label>
            <input
              id={idleId}
              className="input w-full"
              type="number"
              min={0}
              max={1440}
              value={idle}
              aria-invalid={idleProblem ? true : undefined}
              aria-describedby={`${idleId}-hint`}
              onChange={(event) => setIdle(event.target.value)}
            />
            <p id={`${idleId}-hint`} className={idleProblem ? problemClass : hintClass}>
              {idleProblem ?? '0 = never, only for the shared cluster'}
            </p>
          </div>

          <div>
            <div className="mb-1.5 flex items-center justify-between">
              <span id={envId} className="label">Environment</span>
              <button
                type="button"
                className="btn-ghost px-2.5 py-1 text-xs"
                onClick={() => setEnv((rows) => [...rows, { name: '', value: '' }])}
              >
                Add variable
              </button>
            </div>
            {env.length === 0 && <p className={hintClass}>none; values appear in the pods, not in this form again</p>}
            <div className="space-y-2" aria-labelledby={envId}>
              {env.map((row, index) => (
                <div key={index}>
                  <div className="flex gap-2">
                    <input
                      className="input w-1/2"
                      placeholder="NAME"
                      aria-label={`Environment variable ${index + 1} name`}
                      value={row.name}
                      autoComplete="off"
                      spellCheck={false}
                      aria-invalid={envProblems[index] ? true : undefined}
                      onChange={(event) =>
                        setEnv((rows) =>
                          rows.map((r, i) => (i === index ? { ...r, name: event.target.value } : r)),
                        )
                      }
                    />
                    <input
                      className="input flex-1"
                      placeholder="value"
                      aria-label={`Environment variable ${index + 1} value`}
                      value={row.value}
                      autoComplete="off"
                      spellCheck={false}
                      onChange={(event) =>
                        setEnv((rows) =>
                          rows.map((r, i) => (i === index ? { ...r, value: event.target.value } : r)),
                        )
                      }
                    />
                    <button
                      type="button"
                      className="btn-ghost px-2.5 py-1 text-xs"
                      aria-label={`Remove environment variable ${index + 1}`}
                      onClick={() => setEnv((rows) => rows.filter((_, i) => i !== index))}
                    >
                      Remove
                    </button>
                  </div>
                  {envProblems[index] && <p className={problemClass}>{envProblems[index]}</p>}
                </div>
              ))}
            </div>
          </div>

          <div>
            <div className="mb-1.5 flex items-center justify-between">
              <span id={confId} className="label">Spark settings</span>
              <button
                type="button"
                className="btn-ghost px-2.5 py-1 text-xs"
                onClick={() => setConf((rows) => [...rows, { name: '', value: '' }])}
              >
                Add setting
              </button>
            </div>
            {conf.length === 0 && <p className={hintClass}>none; the runtime&rsquo;s defaults apply</p>}
            <div className="space-y-2" aria-labelledby={confId}>
              {conf.map((row, index) => (
                <div key={index}>
                  <div className="flex gap-2">
                    <input
                      className="input w-1/2"
                      placeholder="spark.executor.memory"
                      aria-label={`Spark setting ${index + 1} key`}
                      value={row.name}
                      autoComplete="off"
                      spellCheck={false}
                      aria-invalid={confProblems[index] ? true : undefined}
                      onChange={(event) =>
                        setConf((rows) =>
                          rows.map((r, i) => (i === index ? { ...r, name: event.target.value } : r)),
                        )
                      }
                    />
                    <input
                      className="input flex-1"
                      placeholder="value"
                      aria-label={`Spark setting ${index + 1} value`}
                      value={row.value}
                      autoComplete="off"
                      spellCheck={false}
                      onChange={(event) =>
                        setConf((rows) =>
                          rows.map((r, i) => (i === index ? { ...r, value: event.target.value } : r)),
                        )
                      }
                    />
                    <button
                      type="button"
                      className="btn-ghost px-2.5 py-1 text-xs"
                      aria-label={`Remove spark setting ${index + 1}`}
                      onClick={() => setConf((rows) => rows.filter((_, i) => i !== index))}
                    >
                      Remove
                    </button>
                  </div>
                  {confProblems[index] && <p className={problemClass}>{confProblems[index]}</p>}
                </div>
              ))}
            </div>
          </div>

          {error && (
            <p role="alert" className="text-sm text-bad">{error}</p>
          )}
        </div>

        <div className="flex flex-wrap items-center justify-end gap-2 border-t border-border px-5 py-3">
          {editing && !cluster.shared && (
            <button
              type="button"
              className="btn-ghost mr-auto text-bad"
              disabled={saving || deleting}
              onClick={() => void remove()}
            >
              {confirmDelete ? 'Confirm delete' : 'Delete'}
            </button>
          )}
          <button type="button" className="btn-ghost" onClick={onClose} disabled={saving || deleting}>
            Cancel
          </button>
          <button type="submit" className="btn-primary" disabled={!canSave}>
            {saving ? 'Saving…' : 'Save'}
          </button>
        </div>
      </form>
    </dialog>
  )
}
