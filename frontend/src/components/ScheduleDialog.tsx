import { useEffect, useId, useMemo, useRef, useState, type FormEvent, type ReactNode } from 'react'
import { useQuery } from '@tanstack/react-query'
import {
  api,
  errorMessage,
  type PipelineDetail,
  type PipelineView,
} from '../api'
import {
  browserTimezone,
  cronFor,
  parseSchedule,
  presetForCron,
  suggestName,
  timezoneChoices,
  validateCron,
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

export default function ScheduleDialog({
  mode,
  sql,
  pipeline,
  onClose,
  onScheduled,
}: {
  mode: 'create' | 'edit'
  /** Create mode: the SQL editor's statement when the dialog opened. */
  sql?: string
  /** Edit mode: the pipeline whose statement and schedule change. */
  pipeline?: PipelineDetail
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
  const timeId = `${prefix}-time`
  const weekdayId = `${prefix}-weekday`
  const customId = `${prefix}-custom`
  const customHintId = `${prefix}-custom-hint`
  const timezoneId = `${prefix}-timezone`
  const statementId = `${prefix}-statement`
  const counterId = `${prefix}-counter`
  const nameHintId = `${prefix}-name-hint`
  const inFlight = useRef(false)

  const seed = useMemo(() => whenFrom(pipeline), [pipeline])
  const initialStatement = useMemo(
    () => (editing ? statementFrom(pipeline) : { sql: sql ?? '', sent: true }),
    [editing, pipeline, sql],
  )

  const [name, setName] = useState(() => (editing ? pipeline?.name ?? '' : suggestName(sql ?? '')))
  const [nameTouched, setNameTouched] = useState(editing)
  const [group, setGroup] = useState(() => (editing ? pipeline?.ownerGroup ?? '' : ''))
  const [hoveredGroup, setHoveredGroup] = useState<string | null>(null)
  const [when, setWhen] = useState<WhenPreset>(seed.preset)
  const [time, setTime] = useState(seed.time)
  const [weekday, setWeekday] = useState(seed.weekday)
  const [custom, setCustom] = useState(seed.custom)
  const [timezone, setTimezone] = useState(() => {
    const parsed = parseSchedule(pipeline?.schedule)
    return parsed.timezone ?? browserTimezone()
  })
  const [statement, setStatement] = useState(initialStatement.sql)
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const principalsQuery = useQuery({
    queryKey: ['pipeline-principals'],
    queryFn: api.pipelinePrincipals,
    enabled: !editing,
    staleTime: 30_000,
  })
  const groups = useMemo(() => principalsQuery.data?.groups ?? [], [principalsQuery.data])

  // The first group whose principal is ready is the default, so the common case schedules
  // without a detour. When none is ready the first group is shown to explain why.
  useEffect(() => {
    if (editing || group !== '') return
    const first = groups.find((candidate) => candidate.ready) ?? groups[0]
    if (first) setGroup(first.group)
  }, [editing, group, groups])

  // While the name is untouched it follows the statement's first words.
  const suggested = useMemo(() => suggestName(statement), [statement])
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

  const disabled =
    submitting ||
    !nameOk ||
    !principalsUsable ||
    !groupReady ||
    statementText === '' ||
    (editing && !group) ||
    !scheduleOk

  async function submit(event: FormEvent) {
    event.preventDefault()
    if (disabled || inFlight.current) return
    inFlight.current = true
    setSubmitting(true)
    setError(null)
    const schedule = wantsSchedule && cron !== null
      ? { cron, timezone }
      : null
    try {
      const view = editing && pipeline
        ? await api.updatePipeline(pipeline.namespace, pipeline.name, { sql: statementText, schedule })
        : await api.createPipeline({ name, ownerGroup: group, sql: statementText, schedule })
      onScheduled(view)
    } catch (e: unknown) {
      // The API's own sentence, kept beside the fields it is about.
      setError(errorMessage(e))
      setSubmitting(false)
      inFlight.current = false
    }
  }

  const title = editing ? 'Edit the schedule' : 'Schedule this statement'
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

        <div>
          <label htmlFor={statementId} className="block">
            <span className="label block mb-1.5">Statement</span>
            <textarea
              id={statementId}
              className="input font-mono"
              rows={5}
              maxLength={MAX_STATEMENT}
              value={statement}
              spellCheck={false}
              aria-describedby={counterId}
              data-autofocus={editing ? '' : undefined}
              onChange={(event) => setStatement(event.target.value)}
            />
          </label>
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

        {group !== '' && (
          <p className="text-sm text-text-dim break-words">
            The statement runs as the principal of the group{' '}
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
