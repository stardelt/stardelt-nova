const KEY = 'stardelt-nova:theme';
export function getTheme() {
    try {
        const stored = localStorage.getItem(KEY);
        if (stored === 'light' || stored === 'dark')
            return stored;
    }
    catch {
        /* ignore */
    }
    return 'dark';
}
export function applyTheme(theme) {
    document.documentElement.dataset.theme = theme;
    try {
        localStorage.setItem(KEY, theme);
    }
    catch {
        /* ignore */
    }
}
export function initTheme() {
    applyTheme(getTheme());
}
