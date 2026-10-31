// The theme library (stardelt-internal/design/themes.md): four palettes, chosen per
// person. Dark blue is the default; the choice is applied on <html data-theme>.
import { useSyncExternalStore } from 'react'

export type Theme = 'dark' | 'light' | 'graphite' | 'silver'

/**
 * The four themes, in the order the Preferences picker shows them: each one's label,
 * one sentence, and three swatches (background, surface, accent) for its card.
 */
export const THEMES: { id: Theme; label: string; description: string; swatch: [string, string, string] }[] = [
  { id: 'dark',     label: 'Dark blue', description: 'The stardelt default.',          swatch: ['rgb(11 14 31)', 'rgb(22 27 53)', 'rgb(105 121 255)'] },
  { id: 'graphite', label: 'Graphite',  description: 'Dark, neutral anthracite.',      swatch: ['rgb(24 25 27)', 'rgb(40 42 46)', 'rgb(122 138 255)'] },
  { id: 'light',    label: 'Light',     description: 'Light with a blue tint.',        swatch: ['rgb(246 247 251)', 'rgb(238 240 247)', 'rgb(74 91 240)'] },
  { id: 'silver',   label: 'Silver',    description: 'Light, neutral gray.',           swatch: ['rgb(240 241 242)', 'rgb(230 232 234)', 'rgb(66 82 225)'] },
]

const KEY = 'stardelt-nova:theme'
const EVENT = 'stardelt-nova:theme-change'
let currentTheme: Theme | null = null

/** One of the four ids. Anything else (a stored value from another version) is not a theme. */
export function isTheme(value: unknown): value is Theme {
  return THEMES.some((theme) => theme.id === value)
}

/** The label the picker and the toggle show for a theme. */
export function themeLabel(theme: Theme): string {
  return THEMES.find((entry) => entry.id === theme)?.label ?? theme
}

/** The theme the top-bar toggle switches to: dark blue pairs with light, graphite with silver. */
export function counterpart(theme: Theme): Theme {
  switch (theme) {
    case 'dark': return 'light'
    case 'light': return 'dark'
    case 'graphite': return 'silver'
    case 'silver': return 'graphite'
  }
}

function storedTheme(): Theme | null {
  try {
    const stored = localStorage.getItem(KEY)
    if (isTheme(stored)) return stored
  } catch {
    /* Private windows and blocked storage: no choice is stored. */
  }
  return null
}

/** Whether this browser holds a theme choice of its own (as opposed to the default). */
export function hasThemeChoice(): boolean {
  return storedTheme() !== null
}

export function getTheme(): Theme {
  if (currentTheme) return currentTheme
  currentTheme = storedTheme() ?? 'dark'
  return currentTheme
}

/**
 * Apply a theme and remember it in this browser. The event lets the shell and the
 * Preferences page agree, whichever of the two changed it.
 */
export function applyTheme(theme: Theme) {
  currentTheme = theme
  document.documentElement.dataset.theme = theme
  try {
    localStorage.setItem(KEY, theme)
  } catch {
    /* ignore */
  }
  window.dispatchEvent(new Event(EVENT))
}

/** The first paint: the stored theme, or dark without writing a choice that was never made. */
export function initTheme() {
  document.documentElement.dataset.theme = getTheme()
}

function subscribe(onChange: () => void) {
  window.addEventListener(EVENT, onChange)
  return () => window.removeEventListener(EVENT, onChange)
}

/** The current theme, re-rendering the caller whenever `applyTheme` runs. */
export function useTheme(): Theme {
  return useSyncExternalStore(subscribe, getTheme, getTheme)
}
