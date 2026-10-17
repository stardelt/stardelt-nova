// Theme: dark (default, stardelt brand) or light. Applied on <html data-theme>.
export type Theme = 'dark' | 'light'

const KEY = 'stardelt-nova:theme'

export function getTheme(): Theme {
  try {
    const stored = localStorage.getItem(KEY)
    if (stored === 'light' || stored === 'dark') return stored
  } catch {
    /* ignore */
  }
  return 'dark'
}

export function applyTheme(theme: Theme) {
  document.documentElement.dataset.theme = theme
  try {
    localStorage.setItem(KEY, theme)
  } catch {
    /* ignore */
  }
}

export function initTheme() {
  applyTheme(getTheme())
}
