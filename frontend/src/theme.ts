// Theme: dark (default, stardelt brand) or light. Applied on <html data-theme>.
import { useSyncExternalStore } from 'react'

export type Theme = 'dark' | 'light'

const KEY = 'stardelt-nova:theme'
const EVENT = 'stardelt-nova:theme-change'
let currentTheme: Theme | null = null

function storedTheme(): Theme | null {
  try {
    const stored = localStorage.getItem(KEY)
    if (stored === 'light' || stored === 'dark') return stored
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
