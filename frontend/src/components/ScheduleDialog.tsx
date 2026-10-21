import { useEffect, useId, useMemo, useRef, useState, type FormEvent, type ReactNode } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  api,
  errorMessage,
  type CheckStatementView,
  type NotebookVersion,
  type PipelineDetail,
  type PipelineOptions,
  type PipelineOptionsView,
  type PipelineView,
} from '../api'
import { formatTime, shortVersion } from '../format'
import {
  BACKOFF_CHOICES,
  DEFAULT_OPTIONS,
  MAX_PARAMS,
  MAX_PARAM_VALUE,
  PARAM_NAME_RE,
  RETRY_CHOICES,
  TIMEOUT_CHOICES,
  browserTimezone,
  cronFor,
  namedParameters,
  parameterProblems,
  parameterTable,
  parseSchedule,
  placeholderChips,
  placeholderText,
  presetForCron,
  suggestName,
  timeoutLabel,
  timezoneChoices,
  unknownPlaceholders,
  validateCron,
  withCurrent,
  type ParameterRow,
  type WhenPreset,
  type WhenState,
} from '../schedule'

/**
 * The two dialogs of *Schedule* (stardelt-nova#211): the one that creates or changes a
 * pipeline's cadence, statement and group, and the one destructive confirmation of the
 * pipeline page.
 *
 * Both are native `<dialog>` elements opened with `showModal()`: the browser traps focus,
 * makes the page behind inert and closes on Escape. Focus starts on the first sensible
 * control, returns to the button that opened the dialog, and returns exactly one request for
 * one confirmation.
 *
 * The sentences shown for a refusal are the API's own (`{"error": "…"}`); the cron check is
 * the same function the API applies (`schedule.ts`), so the preview, the inline message and
 * the request cannot disagree.
 *
 * The schedule options (stardelt-nova#223, #224) live in the same dialog: a parameter table,
 * the placeholder chips over the statement, a check that asks the engine to plan it, the next
 * five fire times of the chosen cadence, and the advanced options under a closed `<details>`.
 * The check and the preview never make Create wait: a refusal is the engine's sentence, a
 * preview that fails says so and the person may still schedule.
 *
 * A schedule runs either a statement or a published notebook (stardelt-nova#238). In Notebook
 * mode the statement is replaced by the notebook of the chosen group, the version to pin and
 * the parameters of that version; the request carries `notebook: {name, objectVersion, params}`
 * instead of `sql`. A pipeline never changes kind, so the mode control is disabled when editing.
 */

/** The name rule of the `Pipeline` CRD: 1–40, a leading letter, no trailing dash. */
const NAME_RE = /^[a-z](?:[a-z0-9-]{0,38}[a-z0-9])?$/
const NAME_HINT = '1 to 40 lower-case letters, digits and dashes, starting with a letter'
const NAME_ERROR = 'Use 1 to 40 lower-case letters, digits and dashes, starting with a letter, not ending with a dash.'
const MAX_STATEMENT = 20_000
const NOT_IN_A_GROUP = 'You are not a member of any group, so you cannot schedule.'
const CUSTOM_HINT = 'minute hour day month weekday, for example 0 3 * * *'
const NO_STATEMENT_SENT =
  "This pipeline's statement did not arrive with the pipeline, so it is empty here. Type it to change the schedule."
/** Under the parameter table: the loader's rule in the person's words. */
const PARAMS_HINT = `At most ${MAX_PARAMS}. Names: lower-case letters, digits and underscores.`
/** Over the statement: where the placeholders come from. */
const PLACEHOLDER_HINT = 'Filled in when the run starts.'
const NO_SCHEDULED_RUNS = 'On demand: no scheduled runs'
const CHECK_ACCEPTED = 'Checked: the engine accepts it'
const CHECK_UNCHECKED = 'This kind of statement cannot be checked here'
/** What a schedule can run (stardelt-nova#238): the shared dialog serves both. */
type RunKind = 'sql' | 'notebook'
const RUN_KINDS: ReadonlyArray<{ id: RunKind; label: string }> = [
  { id: 'sql', label: 'SQL statement' },
  { id: 'notebook', label: 'Notebook' },
]
const NO_NOTEBOOK_IN_GROUP =
  'No notebook is published in this group yet. Publish one from JupyterLab with stardelt.publish("file.ipynb").'
const NO_VERSION_FOUND = 'No published version of this notebook was found. Publish it again to schedule it.'
const PIN_HELPER = 'The schedule runs exactly this version. Publishing again does not change it.'
const USE_NEWEST = 'Use newest version'
const NOTEBOOK_VALUE_PROBLEM = `a parameter value is at most ${MAX_PARAM_VALUE} characters`
/** How long the dialog waits after a change before asking for the next runs (ms). */
const PREVIEW_DEBOUNCE = 400
const PREVIEW_COUNT = 5

const PRESETS: ReadonlyArray<{ id: WhenPreset; label: string }> = [
  { id: 'ondemand', label: 'On demand only' },
  { id: 'hourly', label: 'Every hour' },
  { id: 'daily', label: 'Every day at' },
  { id: 'weekdays', label: 'Every weekday at' },
  { id: 'weekly', label: 'Every week on' },
  { id: 'monthly', label: 'Every month on the 1st at' },
  { id: 'custom', label: 'Custom' },
]

const WEEKDAYS: ReadonlyArray<{ value: string; label: string }> = [
  { value: '1', label: 'Monday' },
  { value: '2', label: 'Tuesday' },
  { value: '3', label: 'Wednesday' },
  { value: '4', label: 'Thursday' },
  { value: '5', label: 'Friday' },
  { value: '6', label: 'Saturday' },
  { value: '0', label: 'Sunday' },
]

/** The frame both dialogs share: a heading, a scrollable body and a footer that stays visible. */
function DialogFrame({
  title,
  titleId,
  initialFocus,
  width = 'w-[min(44rem,calc(100vw_-_2rem))]',
  onClose,
  children,
  footer,
}: {
  title: string
  titleId: string
  /** A CSS selector inside the dialog; the first match gets focus on open. */
  initialFocus: string
  width?: string
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
    dialog.querySelector<HTMLElement>(initialFocus)?.focus()
    return () => {
      if (dialog.open) dialog.close()
      opener.current?.focus()
    }
  }, [initialFocus])

  return (
    <dialog
      ref={ref}
      aria-labelledby={titleId}
      onCancel={(event) => {
        event.preventDefault()
        onClose()
      }}
      className={`m-auto ${width} max-w-none rounded-[14px] border border-border bg-bg-elev p-0 text-text shadow-2xl backdrop:bg-black/60`}
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

/**
 * Where the dialog's state starts. Create mode takes the editor's statement; edit mode takes
 * the pipeline's schedule, its group and the statement its `sql` task carries.
 */
function whenFrom(pipeline: PipelineDetail | undefined): WhenState {
  if (!pipeline) return { preset: 'daily', time: '03:00', weekday: '1', custom: '' }
  const parsed = parseSchedule(pipeline.schedule)
  const seed = presetForCron(parsed.cron)
  return {
    preset: seed.preset,
    time: seed.time ?? '03:00',
    weekday: seed.weekday ?? '1',
    custom: seed.custom ?? '',
  }
}

function statementFrom(pipeline: PipelineDetail | undefined): { sql: string; sent: boolean } {
  const task = pipeline?.tasks.find((candidate) => candidate.kind === 'sql')
  return { sql: typeof task?.sql === 'string' ? task.sql : '', sent: typeof task?.sql === 'string' }
}

/** The notebook a stored pipeline pins, as the detail route reports it (#237). */
function notebookFrom(pipeline: PipelineDetail | undefined): {
  name: string
  objectVersion: string
  params: Record<string, string>
} | null {
  const task = pipeline?.tasks.find((candidate) => candidate.kind === 'notebook')
  if (!task) return null
  return {
    name: typeof task.notebook === 'string' ? task.notebook : '',
    objectVersion: typeof task.objectVersion === 'string' ? task.objectVersion : '',
    params: task.notebookParams ?? {},
  }
}

/** What a stored pipeline runs; create mode starts from the control that opened the dialog. */
function runKindFrom(pipeline: PipelineDetail | undefined): RunKind {
  return pipeline?.tasks.some((candidate) => candidate.kind === 'notebook') ? 'notebook' : 'sql'
}

/** A version choice as a person reads it: short id, when it was published, by whom. */
function versionText(version: NotebookVersion): string {
  return `${shortVersion(version.objectVersion)} · ${formatTime(version.uploadedAt)} · ${version.uploadedBy}`
}

/** The advanced options the dialog edits; `Forbid` is one run at a time. */
type OptionState = {
  retries: number
  backoffMinutes: number
  timeoutMinutes: number
  concurrency: 'Allow' | 'Forbid'
}

/**
 * Edit mode starts from the resource's options, defaults filled in; create mode and a
 * backend that predates the field start from the defaults.
 */
function optionsFrom(pipeline: PipelineDetail | undefined): OptionState {
  const view: PipelineOptionsView | null | undefined = pipeline?.options
  return {
    retries: view?.retries ?? DEFAULT_OPTIONS.retries,
    backoffMinutes: view?.backoffMinutes ?? DEFAULT_OPTIONS.backoffMinutes,
    timeoutMinutes: view?.timeoutMinutes ?? DEFAULT_OPTIONS.timeoutMinutes,
    concurrency: view?.concurrency === 'Allow' ? 'Allow' : 'Forbid',
  }
}

/** The parameter table as rows, by name, so edit mode shows it as the resource holds it. */
function parameterRows(pipeline: PipelineDetail | undefined): ParameterRow[] {
  const params = pipeline?.params ?? {}
  return Object.keys(params).sort().map((name) => ({ name, value: params[name] }))
}

/** One row of the dialog, with a key that survives edits and removals. */
type ParamRow = ParameterRow & { key: number }

/** What the engine said about the statement; a change to either input drops it. */
type CheckOutcome =
  | { kind: 'idle' }
  | { kind: 'checking' }
  | { kind: 'accepted' }
  | { kind: 'refused'; message: string }
  | { kind: 'unchecked' }
  | { kind: 'failed'; message: string }

export default function ScheduleDialog({
  mode,
  sql,
  pipeline,
  initialKind = 'sql',
  onClose,
  onScheduled,
}: {
  mode: 'create' | 'edit'
  /** Create mode: the SQL editor's statement when the dialog opened. */
  sql?: string
  /** Edit mode: the pipeline whose statement and schedule change. */
  pipeline?: PipelineDetail
  /** Create mode: the Pipelines page opens a Notebook schedule (#238). */
  initialKind?: RunKind
  onClose: () => void
  onScheduled: (view: PipelineView) => void
}) {
  const editing = mode === 'edit'
  const prefix = useId()
  const titleId = `${prefix}-title`
  const formId = `${prefix}-form`
  const nameId = `${prefix}-name`
  const groupId = `${prefix}-group`
  const whenName = `${prefix}-when`
  const runKindName = `${prefix}-run-kind`
  const notebookId = `${prefix}-notebook`
  const versionId = `${prefix}-version`
  const versionHintId = `${prefix}-version-hint`
  const notebookParamsHintId = `${prefix}-notebook-params-hint`
  const timeId = `${prefix}-time`
  const weekdayId = `${prefix}-weekday`
  const customId = `${prefix}-custom`
  const customHintId = `${prefix}-custom-hint`
  const timezoneId = `${prefix}-timezone`
  const statementId = `${prefix}-statement`
  const counterId = `${prefix}-counter`
  const nameHintId = `${prefix}-name-hint`
  const placeholderHintId = `${prefix}-placeholder-hint`
  const paramsHintId = `${prefix}-params-hint`
  const nextRunsId = `${prefix}-next-runs`
  const runsName = `${prefix}-runs`
  const retriesId = `${prefix}-retries`
  const backoffId = `${prefix}-backoff`
  const timeoutId = `${prefix}-timeout`
  const inFlight = useRef(false)
  const nextKey = useRef(1)
  const statementRef = useRef<HTMLTextAreaElement | null>(null)

  const seed = useMemo(() => whenFrom(pipeline), [pipeline])
  const initialStatement = useMemo(
    () => (editing ? statementFrom(pipeline) : { sql: sql ?? '', sent: true }),
    [editing, pipeline, sql],
  )
  const storedNotebook = useMemo(() => notebookFrom(pipeline), [pipeline])
  /** The caret the last edit left in the statement; a chip inserts there. */
  const caret = useRef({ start: initialStatement.sql.length, end: initialStatement.sql.length })

  const [name, setName] = useState(() => (editing ? pipeline?.name ?? '' : suggestName(sql ?? '')))
  const [nameTouched, setNameTouched] = useState(editing)
  const [group, setGroup] = useState(() => (editing ? pipeline?.ownerGroup ?? '' : ''))
  const [hoveredGroup, setHoveredGroup] = useState<string | null>(null)
  const [runKind, setRunKind] = useState<RunKind>(() => (editing ? runKindFrom(pipeline) : initialKind))
  const [notebookName, setNotebookName] = useState(() => storedNotebook?.name ?? '')
  const [version, setVersion] = useState(() => storedNotebook?.objectVersion ?? '')
  const [notebookValues, setNotebookValues] = useState<Record<string, string>>({})
  const [when, setWhen] = useState<WhenPreset>(seed.preset)
  const [time, setTime] = useState(seed.time)
  const [weekday, setWeekday] = useState(seed.weekday)
  const [custom, setCustom] = useState(seed.custom)
  const [timezone, setTimezone] = useState(() => {
    const parsed = parseSchedule(pipeline?.schedule)
    return parsed.timezone ?? browserTimezone()
  })
  const [statement, setStatement] = useState(initialStatement.sql)
  const [params, setParams] = useState<ParamRow[]>(() =>
    parameterRows(pipeline).map((row) => ({ ...row, key: nextKey.current++ })),
  )
  const [options, setOptions] = useState<OptionState>(() => optionsFrom(pipeline))
  const [check, setCheck] = useState<CheckOutcome>({ kind: 'idle' })
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const notebookMode = runKind === 'notebook'

  const principalsQuery = useQuery({
    queryKey: ['pipeline-principals'],
    queryFn: api.pipelinePrincipals,
    enabled: !editing,
    staleTime: 30_000,
  })
  const groups = useMemo(() => principalsQuery.data?.groups ?? [], [principalsQuery.data])

  // The published notebooks of every group the caller may use; the dialog shows the chosen
  // group's ones. Only asked for in Notebook mode, so the statement dialog is unchanged.
  const notebooksQuery = useQuery({
    queryKey: ['notebooks'],
    queryFn: api.notebooks,
    enabled: notebookMode,
    staleTime: 30_000,
  })
  const groupNotebooks = useMemo(
    () => (Array.isArray(notebooksQuery.data) ? notebooksQuery.data : []).filter((row) => row.group === group),
    [notebooksQuery.data, group],
  )

  const versionsQuery = useQuery({
    queryKey: ['notebook-versions', group, notebookName],
    queryFn: () => api.notebookVersions(group, notebookName),
    enabled: notebookMode && group !== '' && notebookName !== '',
    staleTime: 30_000,
  })
  const versions = useMemo(
    () => (Array.isArray(versionsQuery.data) ? versionsQuery.data : []),
    [versionsQuery.data],
  )

  // `version === ''` in edit mode is the "Use newest version" choice: the parameters route then
  // reads the latest and names it, so the pin is still a concrete id and never "latest at run
  // time". In create mode the newest version is selected as soon as the list arrives.
  const paramsVersion = version === '' ? undefined : version
  const paramsQuery = useQuery({
    queryKey: ['notebook-parameters', group, notebookName, paramsVersion ?? 'newest'],
    queryFn: () => api.notebookParameters(group, notebookName, paramsVersion),
    enabled: notebookMode && group !== '' && notebookName !== '' && (version !== '' || editing),
    staleTime: 30_000,
  })
  const paramsAnswer = paramsQuery.data
  const notebookParameters = useMemo(
    () => (Array.isArray(paramsAnswer?.parameters) ? paramsAnswer.parameters : []),
    [paramsAnswer],
  )

  // The first group whose principal is ready is the default, so the common case schedules
  // without a detour. When none is ready the first group is shown to explain why.
  useEffect(() => {
    if (editing || group !== '') return
    const first = groups.find((candidate) => candidate.ready) ?? groups[0]
    if (first) setGroup(first.group)
  }, [editing, group, groups])

  // Create mode: the first notebook of the group and the newest version are the defaults. A
  // group change that leaves the chosen name behind (or has no notebook at all) falls back
  // instead of keeping a name that group does not have.
  useEffect(() => {
    if (editing || !notebookMode || !notebooksQuery.isSuccess) return
    if (groupNotebooks.length === 0) {
      if (notebookName !== '') {
        setNotebookName('')
        setVersion('')
      }
      return
    }
    if (!groupNotebooks.some((candidate) => candidate.name === notebookName)) {
      setNotebookName(groupNotebooks[0].name)
      setVersion('')
    }
  }, [editing, groupNotebooks, notebookMode, notebookName, notebooksQuery.isSuccess])

  useEffect(() => {
    if (editing || !notebookMode || !versionsQuery.isSuccess) return
    if (versions.length === 0) {
      if (version !== '') setVersion('')
      return
    }
    if (!versions.some((candidate) => candidate.objectVersion === version)) {
      setVersion(versions[0].objectVersion)
    }
  }, [editing, notebookMode, version, versions, versionsQuery.isSuccess])

  // The values start as the tagged cell's defaults; in edit mode the stored values win for the
  // pinned version. A version or notebook change reseeds from that version's defaults.
  const seedKey = useRef<string | null>(null)
  const paramsKey = paramsAnswer ? `${group}\u0000${notebookName}\u0000${paramsAnswer.objectVersion}` : ''
  useEffect(() => {
    if (!paramsAnswer || seedKey.current === paramsKey) return
    seedKey.current = paramsKey
    const stored = editing && storedNotebook && paramsAnswer.objectVersion === storedNotebook.objectVersion
      ? storedNotebook.params
      : {}
    setNotebookValues(Object.fromEntries(
      paramsAnswer.parameters.map((parameter) => [parameter.name, stored[parameter.name] ?? parameter.default]),
    ))
  }, [editing, paramsAnswer, paramsKey, storedNotebook])

  // While the name is untouched it follows the statement's first words, or the notebook's name.
  const suggested = useMemo(
    () => (notebookMode ? suggestName(notebookName.replace(/\.ipynb$/i, '')) : suggestName(statement)),
    [notebookMode, notebookName, statement],
  )
  useEffect(() => {
    if (editing || nameTouched) return
    setName(suggested)
  }, [editing, nameTouched, suggested])

  const zones = useMemo(() => {
    const choices = timezoneChoices()
    return choices.includes(timezone) ? choices : [timezone, ...choices]
  }, [timezone])

  const selected = groups.find((candidate) => candidate.group === group) ?? null
  const hovered = groups.find((candidate) => candidate.group === hoveredGroup) ?? null
  const groupMessage = selected && !selected.ready
    ? selected.message
    : hovered && !hovered.ready
      ? hovered.message
      : null

  const wantsSchedule = when !== 'ondemand'
  const cron = cronFor({ preset: when, time, weekday, custom })
  // Custom text is checked exactly as the API checks it; a preset is built valid, but it is
  // validated too so a broken preview cannot be sent as a surprise refusal.
  const cronError = when === 'custom'
    ? validateCron(custom)
    : cron === null
      ? null
      : validateCron(cron)
  const nameOk = NAME_RE.test(name)
  const statementText = statement.trim()
  const principalsUsable = editing || (!principalsQuery.isLoading && !principalsQuery.isError && groups.length > 0)
  const groupReady = editing || (selected?.ready ?? false)
  const scheduleOk = !wantsSchedule || (cron !== null && cronError === null && timezone !== '')

  // The parameter table and the placeholders: the same rules the API applies (#223). Names in
  // the table that are outside the loader's rule are not offered as chips and do not count as
  // defined, so a statement cannot look valid while the API would refuse the write.
  const written = namedParameters(params)
  const definedNames = written
    .map((row) => row.name)
    .filter((paramName) => PARAM_NAME_RE.test(paramName))
  const chips = placeholderChips(definedNames)
  const problems = parameterProblems(params)
  const parametersOk = problems.length === 0
  const table = parameterTable(params)
  const unknown = unknownPlaceholders(statement, definedNames)

  // Notebook mode (#238): the values are the tagged cell's parameters, the pin is the exact
  // version the parameters route answered. A version the route has not answered for yet blocks
  // Create, so nothing is ever sent as "latest at run time".
  const notebookParamTable = useMemo(
    () => Object.fromEntries(
      notebookParameters.map((parameter) => [parameter.name, notebookValues[parameter.name] ?? parameter.default]),
    ),
    [notebookParameters, notebookValues],
  )
  const notebookValuesOk = Object.values(notebookParamTable)
    .every((value) => value.length <= MAX_PARAM_VALUE && !value.includes('\0'))
  const paramsSettled = paramsQuery.isSuccess && paramsAnswer !== undefined
    && (version === '' || paramsAnswer.objectVersion === version)
  const pinnedVersion = paramsSettled ? paramsAnswer.objectVersion : ''
  const notebookOptions = useMemo(() => {
    const names = groupNotebooks.map((row) => row.name)
    if (editing && notebookName !== '' && !names.includes(notebookName)) names.unshift(notebookName)
    return names
  }, [editing, groupNotebooks, notebookName])
  const versionOptions = useMemo(() => {
    const choices = versions.map((candidate) => ({
      objectVersion: candidate.objectVersion,
      text: versionText(candidate),
    }))
    if (version !== '' && !choices.some((candidate) => candidate.objectVersion === version)) {
      choices.unshift({ objectVersion: version, text: shortVersion(version) })
    }
    return choices
  }, [version, versions])

  // A verdict is about one statement with one parameter table; changing either drops it.
  const checkSignature = JSON.stringify([statementText, table])
  useEffect(() => {
    setCheck((current) => (current.kind === 'idle' ? current : { kind: 'idle' }))
  }, [checkSignature])

  // The next runs follow the fields after a pause, not on every keystroke: one preview request
  // per settled cadence. On demand has no runs to ask about.
  const [previewInput, setPreviewInput] = useState<{ cron: string; timezone: string } | null>(null)
  useEffect(() => {
    if (!wantsSchedule || cron === null || cronError !== null || timezone === '') {
      setPreviewInput(null)
      return
    }
    const timer = window.setTimeout(() => setPreviewInput({ cron, timezone }), PREVIEW_DEBOUNCE)
    return () => window.clearTimeout(timer)
  }, [wantsSchedule, cron, cronError, timezone])

  const preview = useQuery({
    queryKey: ['schedule-preview', previewInput?.cron ?? '', previewInput?.timezone ?? ''],
    queryFn: () =>
      previewInput
        ? api.previewSchedule({ cron: previewInput.cron, timezone: previewInput.timezone, count: PREVIEW_COUNT })
        : Promise.resolve({ runs: [] }),
    enabled: previewInput !== null,
    staleTime: 60_000,
    retry: false,
  })
  const previewRuns = Array.isArray(preview.data?.runs) ? preview.data.runs : []

  function updateParameter(key: number, field: 'name' | 'value', value: string) {
    setParams((rows) => rows.map((row) => (row.key === key ? { ...row, [field]: value } : row)))
  }

  function addParameter() {
    setParams((rows) =>
      rows.length >= MAX_PARAMS ? rows : [...rows, { key: nextKey.current++, name: '', value: '' }],
    )
  }

  function removeParameter(key: number) {
    setParams((rows) => rows.filter((row) => row.key !== key))
  }

  function updateNotebookValue(paramName: string, value: string) {
    setNotebookValues((current) => ({ ...current, [paramName]: value }))
  }

  /** Remember where the caret is, so a chip inserts there rather than at the end. */
  function rememberCaret() {
    const field = statementRef.current
    if (!field) return
    caret.current = { start: field.selectionStart ?? 0, end: field.selectionEnd ?? 0 }
  }

  /** Insert a placeholder's text at the caret and put the caret after it (text only). */
  function insertPlaceholder(text: string) {
    const field = statementRef.current
    const length = statement.length
    const start = Math.min(Math.max(caret.current.start, 0), length)
    const end = Math.min(Math.max(caret.current.end, start), length)
    const next = (statement.slice(0, start) + text + statement.slice(end)).slice(0, MAX_STATEMENT)
    const at = Math.min(start + text.length, next.length)
    setStatement(next)
    caret.current = { start: at, end: at }
    requestAnimationFrame(() => {
      field?.focus()
      field?.setSelectionRange(at, at)
      caret.current = { start: at, end: at }
    })
  }

  async function checkStatement() {
    if (check.kind === 'checking' || statementText === '' || !parametersOk || unknown.length > 0) return
    setCheck({ kind: 'checking' })
    try {
      const answer: CheckStatementView = await api.checkStatement({ sql: statementText, params: table })
      if (!answer.checked) setCheck({ kind: 'unchecked' })
      else if (answer.ok) setCheck({ kind: 'accepted' })
      else setCheck({ kind: 'refused', message: answer.message ?? 'the statement was refused' })
    } catch (e: unknown) {
      // The check is advice, never a gate: the sentence stays beside the statement, Create is
      // unaffected.
      setCheck({ kind: 'failed', message: errorMessage(e) })
    }
  }

  const disabled =
    submitting ||
    !nameOk ||
    !principalsUsable ||
    !groupReady ||
    (editing && !group) ||
    !scheduleOk ||
    (notebookMode
      ? notebookName === '' || pinnedVersion === '' || !notebookValuesOk
      : statementText === '' || !parametersOk || unknown.length > 0)

  async function submit(event: FormEvent) {
    event.preventDefault()
    if (disabled || inFlight.current) return
    inFlight.current = true
    setSubmitting(true)
    setError(null)
    const schedule = wantsSchedule && cron !== null
      ? { cron, timezone }
      : null
    // `PUT` describes the whole editable state: an omitted `params` would remove the table and
    // an omitted option would reset it. The dialog always sends all four fields.
    const optionsBody: PipelineOptions = {
      retries: options.retries,
      backoffMinutes: options.backoffMinutes,
      timeoutMinutes: options.timeoutMinutes,
      concurrency: options.concurrency,
    }
    // Exactly one of the statement and the notebook (#237): the API refuses both or neither.
    const notebookBody = {
      name: notebookName,
      objectVersion: pinnedVersion,
      params: notebookParamTable,
    }
    try {
      const view = editing && pipeline
        ? await api.updatePipeline(pipeline.namespace, pipeline.name, notebookMode
            ? { notebook: notebookBody, schedule, options: optionsBody }
            : { sql: statementText, schedule, params: table, options: optionsBody })
        : await api.createPipeline(notebookMode
            ? { name, ownerGroup: group, notebook: notebookBody, schedule, options: optionsBody }
            : { name, ownerGroup: group, sql: statementText, schedule, params: table, options: optionsBody })
      onScheduled(view)
    } catch (e: unknown) {
      // The API's own sentence, kept beside the fields it is about.
      setError(errorMessage(e))
      setSubmitting(false)
      inFlight.current = false
    }
  }

  const title = editing
    ? 'Edit the schedule'
    : notebookMode
      ? 'Schedule this notebook'
      : 'Schedule this statement'
  const submitLabel = editing ? 'Save schedule' : 'Create schedule'

  return (
    <DialogFrame
      title={title}
      titleId={titleId}
      initialFocus="[data-autofocus]"
      onClose={onClose}
      footer={
        <>
          <button type="submit" form={formId} className="btn-primary" disabled={disabled}>
            {submitting ? (editing ? 'Saving…' : 'Creating…') : submitLabel}
          </button>
          <button type="button" className="btn-ghost" onClick={onClose}>Cancel</button>
        </>
      }
    >
      <form id={formId} className="space-y-5" onSubmit={(event) => void submit(event)}>
        <div className="grid gap-4 sm:grid-cols-2">
          <div className="min-w-0">
            <label htmlFor={nameId} className="block">
              <span className="label block mb-1.5">Name</span>
              <input
                id={nameId}
                className="input font-mono"
                value={name}
                maxLength={40}
                readOnly={editing}
                autoComplete="off"
                spellCheck={false}
                aria-describedby={editing ? undefined : nameHintId}
                data-autofocus={editing ? undefined : ''}
                onChange={(event) => {
                  setName(event.target.value)
                  setNameTouched(true)
                }}
              />
            </label>
            {!editing && <p id={nameHintId} className="mt-1 text-xs text-text-mute">{NAME_HINT}</p>}
            {!editing && nameTouched && !nameOk && (
              <p className="mt-1 text-xs text-bad break-words">{NAME_ERROR}</p>
            )}
          </div>

          <div className="min-w-0">
            {editing ? (
              <label htmlFor={groupId} className="block">
                <span className="label block mb-1.5">Group</span>
                <input id={groupId} className="input font-mono" value={group} readOnly />
              </label>
            ) : (
              <>
                {principalsQuery.isLoading ? (
                  <>
                    <span className="label block mb-1.5">Group</span>
                    <p className="text-sm text-text-mute">Reading your groups…</p>
                  </>
                ) : principalsQuery.isError ? (
                  <>
                    <span className="label block mb-1.5">Group</span>
                    <p role="alert" className="text-sm text-bad break-words">
                      {errorMessage(principalsQuery.error)}
                    </p>
                  </>
                ) : groups.length === 0 ? (
                  <p className="text-sm text-bad">{NOT_IN_A_GROUP}</p>
                ) : (
                  <>
                    <label htmlFor={groupId} className="block">
                      <span className="label block mb-1.5">Group</span>
                      <select
                        id={groupId}
                        className="input font-mono"
                        value={group}
                        aria-describedby={groupMessage ? `${prefix}-group-message` : undefined}
                        onMouseLeave={() => setHoveredGroup(null)}
                        onChange={(event) => {
                          setGroup(event.target.value)
                          setHoveredGroup(null)
                        }}
                      >
                        {groups.map((candidate) => (
                          <option
                            key={candidate.group}
                            value={candidate.group}
                            disabled={!candidate.ready}
                            onMouseEnter={() => setHoveredGroup(candidate.group)}
                          >
                            {candidate.group}
                          </option>
                        ))}
                      </select>
                    </label>
                    {groupMessage && (
                      <p id={`${prefix}-group-message`} className="mt-1 text-xs text-text-mute break-words">
                        {groupMessage}
                      </p>
                    )}
                  </>
                )}
              </>
            )}
          </div>
        </div>

        <fieldset>
          <legend className="label mb-2">When</legend>
          <div className="grid gap-x-5 gap-y-2 sm:grid-cols-2">
            {PRESETS.map((preset) => (
              <label key={preset.id} className="flex items-center gap-2 text-sm">
                <input
                  type="radio"
                  name={whenName}
                  value={preset.id}
                  checked={when === preset.id}
                  onChange={() => setWhen(preset.id)}
                />
                <span>{preset.label}</span>
              </label>
            ))}
          </div>
        </fieldset>

        {(when === 'daily' || when === 'weekdays' || when === 'weekly' || when === 'monthly') && (
          <div className="flex flex-wrap items-end gap-x-4 gap-y-3">
            {when === 'weekly' && (
              <label htmlFor={weekdayId} className="block">
                <span className="label block mb-1.5">Weekday</span>
                <select
                  id={weekdayId}
                  className="input w-auto"
                  value={weekday}
                  onChange={(event) => setWeekday(event.target.value)}
                >
                  {WEEKDAYS.map((day) => (
                    <option key={day.value} value={day.value}>{day.label}</option>
                  ))}
                </select>
              </label>
            )}
            <label htmlFor={timeId} className="block">
              <span className="label block mb-1.5">Time of day</span>
              <input
                id={timeId}
                type="time"
                className="input w-auto"
                value={time}
                required
                onChange={(event) => setTime(event.target.value)}
              />
            </label>
            <span className="pb-2 text-xs text-text-mute whitespace-nowrap">{time} {timezone}</span>
          </div>
        )}

        {when === 'custom' && (
          <div>
            <label htmlFor={customId} className="block">
              <span className="label block mb-1.5">Custom schedule</span>
              <input
                id={customId}
                className="input font-mono"
                value={custom}
                autoComplete="off"
                spellCheck={false}
                aria-describedby={customHintId}
                aria-invalid={cronError ? true : undefined}
                onChange={(event) => setCustom(event.target.value)}
              />
            </label>
            <p id={customHintId} className="mt-1 text-xs text-text-mute">{CUSTOM_HINT}</p>
            {cronError && <p className="mt-1 text-xs text-bad break-words">{cronError}</p>}
          </div>
        )}

        {wantsSchedule && cron !== null && cronError === null && (
          <p className="font-mono text-xs text-text-mute break-all">{cron}</p>
        )}

        <label htmlFor={timezoneId} className="block">
          <span className="label block mb-1.5">Time zone</span>
          <select
            id={timezoneId}
            className="input font-mono text-xs"
            value={timezone}
            onChange={(event) => setTimezone(event.target.value)}
          >
            {zones.map((zone) => <option key={zone} value={zone}>{zone}</option>)}
          </select>
        </label>

        <section aria-labelledby={nextRunsId}>
          <h3 id={nextRunsId} className="label mb-1.5">Next runs</h3>
          {!wantsSchedule ? (
            <p className="text-sm text-text-mute">{NO_SCHEDULED_RUNS}</p>
          ) : cron === null || cronError !== null || timezone === '' ? (
            <p className="text-sm text-text-mute">The next runs appear once the schedule is valid.</p>
          ) : preview.isError ? (
            <p role="alert" className="text-sm text-bad break-words">{errorMessage(preview.error)}</p>
          ) : preview.isLoading ? (
            <p className="text-sm text-text-mute">Reading the next runs…</p>
          ) : previewRuns.length === 0 ? (
            <p className="text-sm text-text-mute">{NO_SCHEDULED_RUNS}</p>
          ) : (
            <ol className="space-y-0.5">
              {previewRuns.map((run) => (
                <li key={run} className="font-mono text-xs text-text-dim break-all">{formatTime(run, timezone)}</li>
              ))}
            </ol>
          )}
        </section>

        <fieldset>
          <legend className="label mb-2">What to run</legend>
          <div className="inline-flex max-w-full flex-wrap rounded-[10px] border border-border bg-bg-soft/40 p-0.5">
            {RUN_KINDS.map((kind) => (
              <label key={kind.id} className="relative">
                <input
                  type="radio"
                  className="peer sr-only"
                  name={runKindName}
                  value={kind.id}
                  checked={runKind === kind.id}
                  disabled={editing}
                  onChange={() => setRunKind(kind.id)}
                />
                <span className="inline-flex cursor-pointer items-center rounded-[8px] px-3 py-1.5 text-sm text-text-dim peer-checked:bg-accent/15 peer-checked:text-text peer-checked:shadow-sm peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-accent peer-disabled:cursor-not-allowed peer-disabled:opacity-60">
                  {kind.label}
                </span>
              </label>
            ))}
          </div>
        </fieldset>

        {!notebookMode && (
          <>
        <div>
          <div className="flex flex-wrap items-center justify-between gap-2">
            <label htmlFor={statementId} className="label">Statement</label>
            <button
              type="button"
              className="btn-ghost py-1 px-2 text-xs"
              disabled={statementText === '' || !parametersOk || unknown.length > 0 || check.kind === 'checking'}
              onClick={() => void checkStatement()}
            >
              Check statement
            </button>
          </div>
          <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
            {chips.map((chip) => (
              <button
                key={chip}
                type="button"
                className="badge border-border bg-bg-soft font-mono text-xs text-text-dim hover:text-text"
                title={`Insert ${placeholderText(chip)}`}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => insertPlaceholder(placeholderText(chip))}
              >
                {placeholderText(chip)}
              </button>
            ))}
          </div>
          <p id={placeholderHintId} className="mt-1 text-xs text-text-mute">{PLACEHOLDER_HINT}</p>
          <textarea
            ref={statementRef}
            id={statementId}
            className="input font-mono mt-2"
            rows={5}
            maxLength={MAX_STATEMENT}
            value={statement}
            spellCheck={false}
            aria-describedby={`${placeholderHintId} ${counterId}`}
            data-autofocus={editing ? '' : undefined}
            onSelect={rememberCaret}
            onClick={rememberCaret}
            onKeyUp={rememberCaret}
            onFocus={rememberCaret}
            onChange={(event) => {
              setStatement(event.target.value)
              caret.current = {
                start: event.target.selectionStart ?? 0,
                end: event.target.selectionEnd ?? 0,
              }
            }}
          />
          {unknown.length > 0 && (
            <div role="alert" className="mt-1 text-xs text-bad">
              <ul className="space-y-0.5">
                {unknown.map((placeholder) => (
                  <li key={placeholder} className="break-words">
                    unknown placeholder <span className="font-mono">{placeholderText(placeholder)}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}
          {check.kind === 'checking' && (
            <p className="mt-1 text-xs text-text-mute">Checking the statement…</p>
          )}
          {check.kind === 'accepted' && (
            <p role="status" className="mt-1 text-xs text-ok break-words">{CHECK_ACCEPTED}</p>
          )}
          {check.kind === 'unchecked' && (
            <p role="status" className="mt-1 text-xs text-text-mute break-words">{CHECK_UNCHECKED}</p>
          )}
          {check.kind === 'refused' && (
            <p role="alert" className="mt-1 text-xs text-bad break-words">The engine says: {check.message}</p>
          )}
          {check.kind === 'failed' && (
            <p role="alert" className="mt-1 text-xs text-bad break-words">{check.message}</p>
          )}
          <div className="mt-1 flex flex-wrap items-start justify-between gap-2">
            {editing && !initialStatement.sent ? (
              <p className="text-xs text-text-mute break-words">{NO_STATEMENT_SENT}</p>
            ) : (
              <span />
            )}
            <p id={counterId} className="text-xs text-text-mute whitespace-nowrap">
              {statement.length} / {MAX_STATEMENT}
            </p>
          </div>
        </div>

        <fieldset className="min-w-0">
          <legend className="label mb-2">Parameters</legend>
          {written.length === 0 && (
            <p className="text-sm text-text-mute">
              No parameters. Add one and use it as a placeholder in the statement.
            </p>
          )}
          {params.length > 0 && (
            <div className="space-y-2">
              {params.map((row, index) => {
                const nameInvalid = row.name === ''
                  ? row.value !== ''
                  : !PARAM_NAME_RE.test(row.name)
                return (
                  <div
                    key={row.key}
                    className="grid grid-cols-1 gap-2 sm:grid-cols-[minmax(0,10rem)_minmax(0,1fr)_auto] sm:items-center"
                  >
                    <input
                      className="input font-mono"
                      aria-label={`Parameter ${index + 1} name`}
                      aria-invalid={nameInvalid ? true : undefined}
                      aria-describedby={paramsHintId}
                      value={row.name}
                      maxLength={32}
                      placeholder="name"
                      autoComplete="off"
                      spellCheck={false}
                      onChange={(event) => updateParameter(row.key, 'name', event.target.value)}
                    />
                    <input
                      className="input font-mono"
                      aria-label={`Parameter ${index + 1} value`}
                      value={row.value}
                      maxLength={256}
                      placeholder="value"
                      autoComplete="off"
                      spellCheck={false}
                      onChange={(event) => updateParameter(row.key, 'value', event.target.value)}
                    />
                    <button
                      type="button"
                      className="btn-ghost py-1.5 px-2 text-xs justify-self-start sm:justify-self-auto"
                      aria-label={`Remove parameter ${index + 1}`}
                      onClick={() => removeParameter(row.key)}
                    >
                      Remove
                    </button>
                  </div>
                )
              })}
            </div>
          )}
          <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1">
            <button
              type="button"
              className="btn-ghost py-1 px-2 text-xs"
              disabled={params.length >= MAX_PARAMS}
              onClick={addParameter}
            >
              Add parameter
            </button>
            <p id={paramsHintId} className="text-xs text-text-mute">{PARAMS_HINT}</p>
          </div>
          {problems.length > 0 && (
            <ul className="mt-1 space-y-0.5 text-xs text-bad">
              {problems.map((problem) => <li key={problem} className="break-words">{problem}</li>)}
            </ul>
          )}
        </fieldset>
          </>
        )}

        {notebookMode && (
          <div className="space-y-4">
            {notebooksQuery.isLoading ? (
              <p className="text-sm text-text-mute">Reading the published notebooks…</p>
            ) : notebooksQuery.isError ? (
              <p role="alert" className="text-sm text-bad break-words">{errorMessage(notebooksQuery.error)}</p>
            ) : notebookOptions.length === 0 ? (
              <p className="text-sm text-text-mute break-words">{NO_NOTEBOOK_IN_GROUP}</p>
            ) : (
              <>
                <label htmlFor={notebookId} className="block">
                  <span className="label block mb-1.5">Notebook</span>
                  <select
                    id={notebookId}
                    className="input font-mono"
                    value={notebookName}
                    data-autofocus={editing ? '' : undefined}
                    onChange={(event) => {
                      setNotebookName(event.target.value)
                      setVersion('')
                    }}
                  >
                    {notebookOptions.map((option) => (
                      <option key={option} value={option}>{option}</option>
                    ))}
                  </select>
                </label>

                {versionsQuery.isError ? (
                  <p role="alert" className="text-sm text-bad break-words">
                    {errorMessage(versionsQuery.error)}
                  </p>
                ) : null}

                {versionOptions.length === 0 ? (
                  <div>
                    <span className="label block mb-1.5">Version</span>
                    <p className="text-sm text-text-mute">
                      {versionsQuery.isLoading ? 'Reading the versions…' : NO_VERSION_FOUND}
                    </p>
                  </div>
                ) : (
                  <div>
                    <label htmlFor={versionId} className="block">
                      <span className="label block mb-1.5">Version</span>
                      <select
                        id={versionId}
                        className="input font-mono text-xs"
                        value={version}
                        aria-describedby={versionHintId}
                        onChange={(event) => setVersion(event.target.value)}
                      >
                        {editing && <option value="">{USE_NEWEST}</option>}
                        {versionOptions.map((option) => (
                          <option key={option.objectVersion} value={option.objectVersion}>{option.text}</option>
                        ))}
                      </select>
                    </label>
                    <p id={versionHintId} className="mt-1 text-xs text-text-mute break-words">{PIN_HELPER}</p>
                  </div>
                )}

                <fieldset className="min-w-0">
                  <legend className="label mb-2">Parameters</legend>
                  {paramsQuery.isError ? (
                    <p role="alert" className="text-sm text-bad break-words">
                      {errorMessage(paramsQuery.error)}
                    </p>
                  ) : notebookParameters.length === 0 ? (
                    <p className="text-sm text-text-mute">
                      {paramsQuery.isLoading ? 'Reading the parameters…' : 'This notebook has no parameters.'}
                    </p>
                  ) : (
                    <div className="space-y-2">
                      {notebookParameters.map((parameter, index) => (
                        <div
                          key={parameter.name}
                          className="grid grid-cols-1 gap-2 sm:grid-cols-[minmax(0,12rem)_minmax(0,1fr)] sm:items-center"
                        >
                          <input
                            className="input font-mono"
                            aria-label={`Parameter ${index + 1} name`}
                            value={parameter.name}
                            readOnly
                            aria-describedby={notebookParamsHintId}
                          />
                          <input
                            className="input font-mono"
                            aria-label={`Parameter ${index + 1} value`}
                            value={notebookValues[parameter.name] ?? parameter.default}
                            maxLength={MAX_PARAM_VALUE}
                            placeholder={parameter.default}
                            autoComplete="off"
                            spellCheck={false}
                            aria-describedby={notebookParamsHintId}
                            onChange={(event) => updateNotebookValue(parameter.name, event.target.value)}
                          />
                        </div>
                      ))}
                    </div>
                  )}
                  {notebookValuesOk ? (
                    <p id={notebookParamsHintId} className="mt-2 text-xs text-text-mute">
                      The values the run passes to the notebook.
                    </p>
                  ) : (
                    <p id={notebookParamsHintId} role="alert" className="mt-2 text-xs text-bad break-words">
                      {NOTEBOOK_VALUE_PROBLEM}
                    </p>
                  )}
                </fieldset>
              </>
            )}
          </div>
        )}

        <details className="rounded-[10px] border border-border bg-bg-soft/30">
          <summary className="cursor-pointer select-none px-3 py-2 text-sm font-medium">Advanced</summary>
          <div className="space-y-4 border-t border-border/70 px-3 py-3">
            <div className="grid gap-4 sm:grid-cols-2">
              <label htmlFor={retriesId} className="block">
                <span className="label block mb-1.5">Retries</span>
                <select
                  id={retriesId}
                  className="input"
                  value={options.retries}
                  onChange={(event) => setOptions((current) => ({ ...current, retries: Number(event.target.value) }))}
                >
                  {withCurrent(RETRY_CHOICES, options.retries).map((value) => (
                    <option key={value} value={value}>
                      {value === 0 ? 'No retries' : `${value} ${value === 1 ? 'retry' : 'retries'}`}
                    </option>
                  ))}
                </select>
              </label>
              <label htmlFor={backoffId} className="block">
                <span className="label block mb-1.5">Wait between retries</span>
                <select
                  id={backoffId}
                  className="input"
                  value={options.backoffMinutes}
                  onChange={(event) => setOptions((current) => ({ ...current, backoffMinutes: Number(event.target.value) }))}
                >
                  {withCurrent(BACKOFF_CHOICES, options.backoffMinutes).map((value) => (
                    <option key={value} value={value}>{value === 1 ? '1 minute' : `${value} minutes`}</option>
                  ))}
                </select>
              </label>
              <label htmlFor={timeoutId} className="block">
                <span className="label block mb-1.5">Time limit</span>
                <select
                  id={timeoutId}
                  className="input"
                  value={options.timeoutMinutes}
                  onChange={(event) => setOptions((current) => ({ ...current, timeoutMinutes: Number(event.target.value) }))}
                >
                  {withCurrent(TIMEOUT_CHOICES.map((choice) => choice.value), options.timeoutMinutes).map((value) => (
                    <option key={value} value={value}>{timeoutLabel(value)}</option>
                  ))}
                </select>
              </label>
            </div>
            <fieldset>
              <legend className="label mb-1.5">Runs</legend>
              <div className="grid gap-2 sm:grid-cols-2">
                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="radio"
                    name={runsName}
                    value="Forbid"
                    checked={options.concurrency === 'Forbid'}
                    onChange={() => setOptions((current) => ({ ...current, concurrency: 'Forbid' }))}
                  />
                  <span>One run at a time</span>
                </label>
                <label className="flex items-center gap-2 text-sm">
                  <input
                    type="radio"
                    name={runsName}
                    value="Allow"
                    checked={options.concurrency === 'Allow'}
                    onChange={() => setOptions((current) => ({ ...current, concurrency: 'Allow' }))}
                  />
                  <span>Allow runs in parallel</span>
                </label>
              </div>
            </fieldset>
            <p className="text-xs text-text-mute">
              A failed attempt is tried again after the wait. The time limit stops a run that takes too long.
            </p>
          </div>
        </details>

        {group !== '' && (
          <p className="text-sm text-text-dim break-words">
            The {notebookMode ? 'notebook' : 'statement'} runs as the principal of the group{' '}
            <span className="font-mono">{group}</span>, so it can only reach what the group may.
          </p>
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

/**
 * The destructive confirmation of the pipeline page: one name, one consequence, one request.
 * Focus starts on Cancel, Escape closes, and a refusal (a pipeline managed outside Nova) stays
 * in this dialog with the API's sentence.
 */
export function DeletePipelineDialog({
  pipeline,
  onClose,
  onDeleted,
}: {
  pipeline: PipelineView
  onClose: () => void
  onDeleted: () => void
}) {
  const prefix = useId()
  const titleId = `${prefix}-title`
  const inFlight = useRef(false)
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function remove() {
    if (inFlight.current) return
    inFlight.current = true
    setSubmitting(true)
    setError(null)
    try {
      await api.deletePipeline(pipeline.namespace, pipeline.name)
      onDeleted()
    } catch (e: unknown) {
      setError(errorMessage(e))
      setSubmitting(false)
      inFlight.current = false
    }
  }

  return (
    <DialogFrame
      title={`Delete ${pipeline.name}?`}
      titleId={titleId}
      initialFocus="[data-autofocus]"
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn-danger" disabled={submitting} onClick={() => void remove()}>
            {submitting ? 'Deleting…' : 'Delete pipeline'}
          </button>
          <button type="button" className="btn-ghost" onClick={onClose} data-autofocus>
            Cancel
          </button>
        </>
      }
    >
      <p className="text-sm text-text-dim break-words">
        The pipeline and its schedule are removed. Past runs stay readable for a while in Airflow
        but disappear from this page.
      </p>
      {error && (
        <div role="alert" className="mt-3 rounded-[10px] border border-red-500/40 bg-red-500/10 p-3 text-sm text-bad break-words">
          {error}
        </div>
      )}
    </DialogFrame>
  )
}
