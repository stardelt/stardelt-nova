import { useId, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { ApiError, api } from '../api'
import ErrorBox from '../components/ErrorBox'
import { getTimeDisplay, setTimeDisplay, type TimeDisplay } from '../format'
import { THEMES, applyTheme, useTheme, type Theme } from '../theme'

/**
 * What a person can change for themselves. The browser keeps the choice at once; when
 * Nova has a state store the same value is saved to the person, so another browser
 * follows it (stardelt-nova#215).
 */

const NO_STORE =
  'Your preferences are kept in this browser only: this installation has no state store.'

/**
 * The state store's key shape is `[a-z0-9][a-z0-9._-]{0,63}` (ticket #124), so the
 * ticket's `timeDisplay` is stored as `time.display`; an uppercase key would be refused.
 */
const TIME_SETTING = 'time.display'

/** The browser's zone in words, or no parenthetical when it does not report one. */
function browserZone(): string | null {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || null
  } catch {
    return null
  }
}

function Choice({
  name,
  checked,
  onChange,
  children,
}: {
  name: string
  checked: boolean
  onChange: () => void
  children: React.ReactNode
}) {
  const id = useId()
  return (
    <label htmlFor={id} className="flex items-center gap-2.5 text-sm">
      <input id={id} type="radio" name={name} className="mt-0" checked={checked} onChange={onChange} />
      <span>{children}</span>
    </label>
  )
}

export default function Preferences() {
  const theme = useTheme()
  const themeGroupId = useId()
  const [time, setTime] = useState<TimeDisplay>(getTimeDisplay)
  const settings = useQuery({ queryKey: ['settings'], queryFn: api.settings, retry: false })
  const noStore = settings.error instanceof ApiError && settings.error.status === 503
  const failed = settings.isError && !noStore
  const zone = browserZone()
  const localLabel = zone ? `Local time (${zone})` : 'Local time'

  function chooseTheme(next: Theme) {
    if (next === theme) return
    applyTheme(next)
    void api.putSetting('theme', next).catch(() => {
      // The choice already applies here; a missing state store only keeps it local.
    })
  }

  function chooseTime(next: TimeDisplay) {
    if (next === time) return
    setTime(next)
    setTimeDisplay(next)
    void api.putSetting(TIME_SETTING, next).catch(() => {
      // The choice already applies here; a missing state store only keeps it local.
    })
  }

  return (
    <div className="max-w-3xl space-y-6">
      <header>
        <h1 className="text-2xl font-bold tracking-tight">Preferences</h1>
        <p className="text-text-dim text-sm mt-1">
          What you can change for yourself. They follow you to other browsers when Nova has a state
          store.
        </p>
      </header>

      {noStore && <p className="text-text-mute text-xs">{NO_STORE}</p>}
      {failed && <ErrorBox error={settings.error} onRetry={() => void settings.refetch()} />}

      <section className="card p-5">
        <fieldset>
          <legend className="label mb-3">Appearance</legend>
          {/* The theme library (stardelt-nova#221): a native radio group, so the arrow
              keys move the choice and Space selects it. The word "Selected" carries the
              choice as well as the accent border (U15). */}
          <div className="grid gap-3 sm:grid-cols-2">
            {THEMES.map((entry) => {
              const labelId = `${themeGroupId}-${entry.id}`
              const selected = theme === entry.id
              return (
                <label
                  key={entry.id}
                  className={`flex items-start gap-3 rounded-[10px] border p-3 transition ${
                    selected ? 'border-accent bg-accent/10' : 'border-border bg-bg-soft/40 hover:border-accent/40'
                  }`}
                >
                  <input
                    type="radio"
                    name="theme"
                    value={entry.id}
                    checked={selected}
                    onChange={() => chooseTheme(entry.id)}
                    aria-labelledby={labelId}
                    className="mt-1 shrink-0 accent-accent focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
                  />
                  <span className="min-w-0">
                    <span className="flex gap-1.5">
                      {entry.swatch.map((colour) => (
                        <span
                          key={colour}
                          aria-hidden="true"
                          className="h-4 w-8 rounded-[4px] border border-border"
                          style={{ backgroundColor: colour }}
                        />
                      ))}
                    </span>
                    <span id={labelId} className="mt-2 block text-sm font-medium">
                      {entry.label}
                    </span>
                    <span className="mt-0.5 block text-xs text-text-dim">{entry.description}</span>
                    {selected && <span className="mt-1.5 block text-[11px] text-accent">Selected</span>}
                  </span>
                </label>
              )
            })}
          </div>
        </fieldset>
      </section>

      <section className="card p-5">
        <fieldset>
          <legend className="label mb-3">Time</legend>
          <div className="space-y-2.5">
            <Choice name="time-display" checked={time === 'local'} onChange={() => chooseTime('local')}>
              {localLabel}
            </Choice>
            <Choice name="time-display" checked={time === 'utc'} onChange={() => chooseTime('utc')}>
              UTC
            </Choice>
          </div>
        </fieldset>
      </section>
    </div>
  )
}
