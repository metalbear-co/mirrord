import { Button, MirrordIcon, SearchInput, cn } from '@metalbear/ui'
import { Moon, Settings, Sun } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { strings } from '../strings'
import SettingsDialog from './SettingsDialog'

import type { ThemePref } from '../theme'

export type HeaderActivity = {
  status: 'Watching' | 'Joined'
  target: string
  uptime?: string
}

interface Props {
  connected: boolean
  isDarkMode: boolean
  onToggleTheme: () => void
  theme: ThemePref
  onThemeChange: (t: ThemePref) => void
  telemetryEnabled: boolean
  onTelemetryChange: (enabled: boolean) => void
  activity?: HeaderActivity | null
  query: string
  onQueryChange: (q: string) => void
}

const isMac = typeof navigator !== 'undefined' && /Mac/i.test(navigator.platform)
const SEARCH_HINT = isMac ? '⌘F' : 'Ctrl F'

export default function AppHeader({
  connected,
  isDarkMode,
  onToggleTheme,
  theme,
  onThemeChange,
  telemetryEnabled,
  onTelemetryChange,
  activity,
  query,
  onQueryChange,
}: Props) {
  const [settingsOpen, setSettingsOpen] = useState(false)
  const searchRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && (e.key === 'f' || e.key === 'F')) {
        e.preventDefault()
        searchRef.current?.focus()
        searchRef.current?.select()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  return (
    <header className="relative shrink-0 bg-background border-b border-border text-foreground shadow-[0_1px_2px_-1px_rgb(0_0_0_/_0.04)]">
      {/* Hair-thin inner top highlight — a soft rim of light that gives the header a
          glassy finish in both themes, replacing the prior brand-colored accent line. */}
      <div className="pointer-events-none absolute inset-x-0 top-0 h-px bg-gradient-to-r from-transparent via-foreground/10 to-transparent" />
      <div className="px-4 sm:px-6 lg:px-8">
        <div className="grid grid-cols-[1fr_auto_1fr] items-center h-14 gap-3">
          <div className="flex items-center gap-3 min-w-0">
            <img
              src={MirrordIcon}
              alt={strings.app.title}
              className={cn('w-8 h-8 shrink-0', isDarkMode && 'invert')}
            />
            <div className="hidden sm:flex items-center gap-2 min-w-0">
              <span className="font-semibold text-h4">{strings.app.title}</span>
              <span className="text-foreground/30">|</span>
              <span className="text-body-sm font-medium text-foreground/70 truncate">
                {strings.app.subtitle}
              </span>
            </div>
            <span className="font-semibold text-h4 sm:hidden">{strings.app.title}</span>
          </div>

          <div className="flex justify-center min-w-0">
            {activity && (
              <div className="inline-flex items-center gap-2 rounded-full border border-border bg-card/50 px-3 py-1 max-w-full">
                <span
                  className={cn(
                    'h-2 w-2 rounded-full shrink-0',
                    activity.status === 'Joined' ? 'bg-primary' : 'bg-emerald-500'
                  )}
                />
                <span className="text-body-sm text-muted-foreground shrink-0">
                  {activity.status}
                </span>
                <span className="font-mono text-body-sm text-foreground truncate">
                  {activity.target}
                </span>
                {activity.uptime && (
                  <>
                    <span className="text-muted-foreground/50 shrink-0">·</span>
                    <span className="text-body-sm text-muted-foreground shrink-0">
                      {activity.uptime}
                    </span>
                  </>
                )}
              </div>
            )}
          </div>

          <div className="flex items-center justify-end gap-2 min-w-0">
            <div className="relative w-44 sm:w-56 hidden md:block">
              <SearchInput
                ref={searchRef}
                value={query}
                onChange={(e) => onQueryChange(e.target.value)}
                onClear={() => onQueryChange('')}
                placeholder="Search"
                className="h-8 pr-12 text-xs"
              />
              {!query && (
                <kbd className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 select-none rounded border border-border bg-muted/50 px-1.5 py-0.5 font-mono text-[10px] leading-none text-muted-foreground">
                  {SEARCH_HINT}
                </kbd>
              )}
            </div>

            <div className="hidden lg:flex items-center gap-2 pl-1">
              <div
                className={cn('h-2 w-2 rounded-full', connected ? 'bg-green-500' : 'bg-red-500')}
              />
              <span className="text-body-sm text-foreground/60">
                {connected ? strings.app.connected : strings.app.disconnected}
              </span>
            </div>

            <Button
              variant="ghost"
              size="icon"
              onClick={onToggleTheme}
              title={isDarkMode ? strings.app.themeLight : strings.app.themeDark}
              className="h-8 w-8 text-foreground/60 hover:text-foreground"
            >
              {isDarkMode ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
            </Button>

            <Button
              variant="ghost"
              size="icon"
              onClick={() => setSettingsOpen(true)}
              title={strings.app.settings}
              className="h-8 w-8 text-foreground/60 hover:text-foreground"
            >
              <Settings className="h-4 w-4" />
            </Button>
          </div>
        </div>
      </div>

      <SettingsDialog
        open={settingsOpen}
        onOpenChange={setSettingsOpen}
        theme={theme}
        onThemeChange={onThemeChange}
        telemetryEnabled={telemetryEnabled}
        onTelemetryChange={onTelemetryChange}
      />
    </header>
  )
}
