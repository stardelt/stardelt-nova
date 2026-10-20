import { useId, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { ApiError, api } from '../api'
import ErrorBox from '../components/ErrorBox'
import { getTimeDisplay, setTimeDisplay, type TimeDisplay } from '../format'
import { applyTheme, useTheme, type Theme } from '../theme'

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
          <div className="space-y-2.5">
            <Choice name="theme" checked={theme === 'dark'} onChange={() => chooseTheme('dark')}>
              Dark
            </Choice>
            <Choice name="theme" checked={theme === 'light'} onChange={() => chooseTheme('light')}>
              Light
            </Choice>
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
