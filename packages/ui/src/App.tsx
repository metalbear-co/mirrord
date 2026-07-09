import { Suspense, lazy, useCallback, useEffect, useState } from 'react'
import { MirrordIcon } from '@metalbear/ui'
import { Moon, Sun } from 'lucide-react'
import {
  applyDark,
  loadTheme,
  resolveDark,
  saveTheme,
  type ThemePref,
} from '@mirrord/monitor/theme'

// Both features live in one app as tabs. Each is lazy-loaded on first activation and then kept
// mounted (hidden when inactive) so switching tabs preserves its state — the monitor keeps its
// live session stream, and an in-progress wizard config isn't lost on a detour to the monitor.
const Monitor = lazy(() => import('@mirrord/monitor'))
const Wizard = lazy(() => import('@mirrord/wizard'))

type Tab = 'monitor' | 'wizard'

const TABS: { id: Tab; label: string; path: string }[] = [
  { id: 'monitor', label: 'Session Monitor', path: '/' },
  { id: 'wizard', label: 'Config Wizard', path: '/wizard' },
]

/** The tab is chosen from the URL so `mirrord ui` (`/`) and `mirrord wizard` (`/wizard`) deep-link. */
function tabForPath(path: string): Tab {
  return path === '/wizard' || path.startsWith('/wizard/')
    ? 'wizard'
    : 'monitor'
}

function TabBar({
  active,
  onSelect,
  isDark,
  onToggleTheme,
}: {
  active: Tab
  onSelect: (tab: Tab) => void
  isDark: boolean
  onToggleTheme: () => void
}) {
  return (
    <header className="flex h-11 shrink-0 items-center gap-1 border-b border-border bg-background px-3">
      <div className="mr-3 flex items-center gap-2">
        <img src={MirrordIcon} alt="" className="h-5 w-5" />
        <span className="text-sm font-semibold text-foreground">mirrord</span>
      </div>
      <nav className="flex items-center gap-1">
        {TABS.map((tab) => {
          const isActive = tab.id === active
          return (
            <button
              key={tab.id}
              type="button"
              onClick={() => onSelect(tab.id)}
              aria-current={isActive ? 'page' : undefined}
              className={`h-11 border-b-2 px-3 text-sm font-medium transition-colors ${
                isActive
                  ? 'border-primary text-foreground'
                  : 'border-transparent text-muted-foreground hover:text-foreground'
              }`}
            >
              {tab.label}
            </button>
          )
        })}
      </nav>
      <button
        type="button"
        onClick={onToggleTheme}
        aria-label={isDark ? 'Switch to light mode' : 'Switch to dark mode'}
        title={isDark ? 'Switch to light mode' : 'Switch to dark mode'}
        className="ml-auto flex h-8 w-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
      >
        {isDark ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
      </button>
    </header>
  )
}

export default function App() {
  const [active, setActive] = useState<Tab>(() =>
    tabForPath(window.location.pathname),
  )
  const [mounted, setMounted] = useState<Set<Tab>>(
    () => new Set([tabForPath(window.location.pathname)]),
  )

  // Theme is owned here so a single top-right toggle controls both tabs. The monitor tab receives
  // the resolved values as props (it no longer keeps its own theme state).
  const [theme, setTheme] = useState<ThemePref>(loadTheme)
  const isDark = resolveDark(theme)

  useEffect(() => {
    applyDark(isDark)
    saveTheme(theme)
  }, [theme, isDark])

  // When the preference is `system`, follow OS light/dark changes live.
  useEffect(() => {
    if (theme !== 'system') return
    const media = window.matchMedia('(prefers-color-scheme: dark)')
    const handler = () => applyDark(media.matches)
    media.addEventListener('change', handler)
    return () => media.removeEventListener('change', handler)
  }, [theme])

  const toggleTheme = useCallback(() => {
    setTheme((prev) => (resolveDark(prev) ? 'light' : 'dark'))
  }, [])

  const selectTab = useCallback((tab: Tab) => {
    setActive(tab)
    setMounted((prev) => (prev.has(tab) ? prev : new Set(prev).add(tab)))
    const path = TABS.find((t) => t.id === tab)?.path ?? '/'
    if (window.location.pathname !== path) {
      window.history.pushState({}, '', path)
    }
  }, [])

  // Keep the active tab in sync with browser back/forward navigation.
  useEffect(() => {
    const onPopState = () => {
      const tab = tabForPath(window.location.pathname)
      setActive(tab)
      setMounted((prev) => (prev.has(tab) ? prev : new Set(prev).add(tab)))
    }
    window.addEventListener('popstate', onPopState)
    return () => window.removeEventListener('popstate', onPopState)
  }, [])

  return (
    <div className="flex h-screen flex-col bg-background text-foreground">
      <TabBar
        active={active}
        onSelect={selectTab}
        isDark={isDark}
        onToggleTheme={toggleTheme}
      />
      <main className="min-h-0 flex-1">
        {mounted.has('monitor') && (
          <div className={active === 'monitor' ? 'h-full' : 'hidden'}>
            <Suspense fallback={null}>
              <Monitor
                theme={theme}
                isDarkMode={isDark}
                onThemeChange={setTheme}
              />
            </Suspense>
          </div>
        )}
        {mounted.has('wizard') && (
          <div
            className={active === 'wizard' ? 'h-full overflow-auto' : 'hidden'}
          >
            <Suspense fallback={null}>
              <Wizard />
            </Suspense>
          </div>
        )}
      </main>
    </div>
  )
}
