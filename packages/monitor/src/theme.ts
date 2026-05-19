export type ThemePref = 'system' | 'light' | 'dark'

const STORAGE_KEY = 'session-monitor-theme'

function isThemePref(v: unknown): v is ThemePref {
  return v === 'system' || v === 'light' || v === 'dark'
}

export function loadTheme(): ThemePref {
  try {
    const v = localStorage.getItem(STORAGE_KEY)
    if (isThemePref(v)) return v
    if (v === 'true') return 'dark'
    if (v === 'false') return 'light'
  } catch {}
  return 'system'
}

export function saveTheme(pref: ThemePref): void {
  try {
    localStorage.setItem(STORAGE_KEY, pref)
  } catch {}
}

export function resolveDark(pref: ThemePref): boolean {
  if (pref === 'dark') return true
  if (pref === 'light') return false
  return window.matchMedia('(prefers-color-scheme: dark)').matches
}

export function applyDark(dark: boolean): void {
  document.documentElement.classList.toggle('dark', dark)
}
