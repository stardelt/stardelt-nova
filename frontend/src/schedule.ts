/**
 * The schedule rules of the backend, in the browser (stardelt-nova#210, #211).
 *
 * `validateCron` is a port of the Rust `validate_cron` in `backend/src/pipelines.rs`: five
 * whitespace-separated fields, ranges checked per field (months 1–12, weekdays 0–7, names
 * accepted), no seconds, no `@daily`, and never more often than one run every five minutes.
 * The sentences are the API's, word for word, so a person sees the same answer before and
 * after the request.
 *
 * A dialog builds its cron from a preset; the cron text is what the request carries, so the
 * preview and the body cannot disagree.
 */

export type WhenPreset =
  | 'ondemand'
  | 'hourly'
  | 'daily'
  | 'weekdays'
  | 'weekly'
  | 'monthly'
  | 'custom'

/** The preset and the controls it uses. `time` is `HH:MM`; `weekday` is `0`–`6`, Sunday first. */
export type WhenState = {
  preset: WhenPreset
  time: string
  weekday: string
  custom: string
}

export const CRON_FIELDS_SENTENCE = 'a schedule has five fields: minute hour day month weekday'
export const CRON_RANGE_SENTENCE = 'a field is out of range'
export const CRON_TOO_OFTEN_SENTENCE = 'at most one run every 5 minutes'

const MONTH_NAMES = ['jan', 'feb', 'mar', 'apr', 'may', 'jun', 'jul', 'aug', 'sep', 'oct', 'nov', 'dec']
const WEEKDAY_NAMES = ['sun', 'mon', 'tue', 'wed', 'thu', 'fri', 'sat']

/** One number or name in a field, mapped to its value; `null` when it is not in range. */
function fieldValue(text: string, min: number, max: number, names: readonly string[]): number | null {
  let value: number
  if (/^\d+$/.test(text)) {
    value = Number(text)
  } else {
    const index = names.findIndex((name) => name.toLowerCase() === text.toLowerCase())
    if (index < 0) return null
    value = min + index
  }
  return value >= min && value <= max ? value : null
}

/** The values one field fires on; `null` when the field is malformed or out of range. */
function fieldValues(field: string, min: number, max: number, names: readonly string[]): number[] | null {
  const values = new Set<number>()
  for (const part of field.split(',')) {
    if (part === '') return null
    const pieces = part.split('/')
    if (pieces.length > 2) return null
    const body = pieces[0]
    let step = 1
    if (pieces.length === 2) {
      if (!/^\d+$/.test(pieces[1])) return null
      step = Number(pieces[1])
      if (step < 1) return null
    }
    let start: number
    let end: number
    if (body === '*') {
      start = min
      end = max
    } else if (body.includes('-')) {
      const bounds = body.split('-')
      if (bounds.length !== 2) return null
      const first = fieldValue(bounds[0], min, max, names)
      const last = fieldValue(bounds[1], min, max, names)
      if (first === null || last === null || first > last) return null
      start = first
      end = last
    } else {
      const single = fieldValue(body, min, max, names)
      if (single === null) return null
      // `a/n` is not one of the shapes the backend accepts: only `* /n` and `a-b/n` take a step.
      if (step !== 1) return null
      start = single
      end = single
    }
    for (let value = start; value <= end; value += step) values.add(value)
  }
  return [...values].sort((a, b) => a - b)
}

/** `true` when two fires are closer than five minutes, or more than twelve fire in an hour. */
function firesTooOften(minutes: number[]): boolean {
  if (minutes.length > 12) return true
  for (let index = 1; index < minutes.length; index += 1) {
    if (minutes[index] - minutes[index - 1] < 5) return true
  }
  if (minutes.length > 1) {
    const first = minutes[0]
    const last = minutes[minutes.length - 1]
    if (60 - last + first < 5) return true
  }
  return false
}

/**
 * The schedule rules of the loader. `null` when the text is accepted, else the sentence the
 * API answers with for the same text.
 */
export function validateCron(text: string): string | null {
  const fields = text.split(/\s+/).filter(Boolean)
  if (fields.length !== 5) return CRON_FIELDS_SENTENCE
  const minutes = fieldValues(fields[0], 0, 59, [])
  if (minutes === null) return CRON_RANGE_SENTENCE
  if (fieldValues(fields[1], 0, 23, []) === null) return CRON_RANGE_SENTENCE
  if (fieldValues(fields[2], 1, 31, []) === null) return CRON_RANGE_SENTENCE
  if (fieldValues(fields[3], 1, 12, MONTH_NAMES) === null) return CRON_RANGE_SENTENCE
  if (fieldValues(fields[4], 0, 7, WEEKDAY_NAMES) === null) return CRON_RANGE_SENTENCE
  if (firesTooOften(minutes)) return CRON_TOO_OFTEN_SENTENCE
  return null
}

/** `HH:MM` as numbers; `null` when it is not a time a browser time input can produce. */
export function parseTime(value: string): { hour: number; minute: number } | null {
  const match = /^(\d{1,2}):(\d{2})$/.exec(value.trim())
  if (!match) return null
  const hour = Number(match[1])
  const minute = Number(match[2])
  return hour <= 23 && minute <= 59 ? { hour, minute } : null
}

function weekdayNumber(value: string): number {
  const day = Number(value)
  return Number.isInteger(day) && day >= 0 && day <= 6 ? day : 1
}

/** The cron the preset builds; `null` for on demand and for an incomplete custom text. */
export function cronFor(when: WhenState): string | null {
  if (when.preset === 'ondemand') return null
  if (when.preset === 'hourly') return '0 * * * *'
  if (when.preset === 'custom') {
    const text = when.custom.trim()
    return text === '' ? null : text
  }
  const clock = parseTime(when.time)
  if (!clock) return null
  const minute = String(clock.minute)
  const hour = String(clock.hour)
  switch (when.preset) {
    case 'daily':
      return `${minute} ${hour} * * *`
    case 'weekdays':
      return `${minute} ${hour} * * 1-5`
    case 'weekly':
      return `${minute} ${hour} * * ${weekdayNumber(when.weekday)}`
    case 'monthly':
      return `${minute} ${hour} 1 * *`
    default:
      return null
  }
}

/**
 * Which preset a stored cron matches, with the controls it needs. A cron the presets cannot
 * express opens as `custom`, so editing never silently rewrites somebody's schedule.
 */
export function presetForCron(cron: string | null): {
  preset: WhenPreset
  time?: string
  weekday?: string
  custom?: string
} {
  const fallback: WhenState = { preset: 'custom', time: '03:00', weekday: '1', custom: cron ?? '' }
  if (!cron) return { ...fallback, preset: 'ondemand', custom: '' }
  const fields = cron.trim().split(/\s+/).filter(Boolean)
  if (fields.length !== 5) return fallback
  const [minuteText, hourText, day, month, weekday] = fields
  if (minuteText === '0' && hourText === '*' && day === '*' && month === '*' && weekday === '*') {
    return { ...fallback, preset: 'hourly' }
  }
  if (!/^\d+$/.test(minuteText) || !/^\d+$/.test(hourText)) return fallback
  const minute = Number(minuteText)
  const hour = Number(hourText)
  if (minute > 59 || hour > 23) return fallback
  const time = `${String(hour).padStart(2, '0')}:${String(minute).padStart(2, '0')}`
  if (day === '*' && month === '*' && weekday === '*') return { ...fallback, preset: 'daily', time }
  if (day === '*' && month === '*' && weekday === '1-5') return { ...fallback, preset: 'weekdays', time }
  if (day === '*' && month === '*' && /^\d+$/.test(weekday) && Number(weekday) <= 7) {
    return { ...fallback, preset: 'weekly', time, weekday: String(Number(weekday) % 7) }
  }
  if (day === '1' && month === '*' && weekday === '*') return { ...fallback, preset: 'monthly', time }
  return fallback
}

/**
 * Split the view's `"0 3 * * * (Europe/Berlin)"` into its cron and zone. A schedule without a
 * zone (a bare cron) keeps the cron and no zone, and the dialog asks for one.
 */
export function parseSchedule(value: string | null | undefined): { cron: string | null; timezone: string | null } {
  if (!value) return { cron: null, timezone: null }
  const withZone = /^(.*?)\s*\(([^()]*)\)\s*$/.exec(value)
  if (withZone) {
    return {
      cron: withZone[1].trim() === '' ? null : withZone[1].trim(),
      timezone: withZone[2].trim() === '' ? null : withZone[2].trim(),
    }
  }
  return { cron: value.trim() === '' ? null : value.trim(), timezone: null }
}

/**
 * A name suggested from the first words of a statement. It always satisfies the name rule
 * (lower-case, a leading letter, at most 40 characters, no trailing dash), and an empty
 * statement suggests nothing.
 */
export function suggestName(sql: string): string {
  const words = sql
    .replace(/\/\*[\s\S]*?\*\//g, ' ')
    .replace(/--[^\n]*/g, ' ')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, ' ')
    .trim()
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 4)
    .join('-')
    .slice(0, 40)
    .replace(/-+$/g, '')
  if (words === '') return ''
  return /^[a-z]/.test(words) ? words : `q-${words}`.slice(0, 40).replace(/-+$/g, '')
}

/** The zone a new schedule starts in: the browser's, else UTC. */
export function browserTimezone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'
  } catch {
    return 'UTC'
  }
}

/**
 * The zone choices: the browser's zone first, then UTC, then every zone the browser knows.
 * A browser without `Intl.supportedValuesOf` still gets the first two.
 */
export function timezoneChoices(): string[] {
  const browser = browserTimezone()
  const supported = (Intl as unknown as { supportedValuesOf?: (key: string) => string[] })
    .supportedValuesOf?.('timeZone') ?? []
  const all = [browser, 'UTC', ...(Array.isArray(supported) ? supported : [])]
  const seen = new Set<string>()
  const zones: string[] = []
  for (const zone of all) {
    if (typeof zone !== 'string' || zone === '' || seen.has(zone)) continue
    seen.add(zone)
    zones.push(zone)
  }
  return zones
}
