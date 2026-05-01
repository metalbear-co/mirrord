import { Button, MirrordIcon, SearchInput, cn } from '@metalbear/ui'
import { ChevronDown, Settings, User } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { strings } from '../strings'
import SettingsDialog from './SettingsDialog'

import type { ThemePref } from '../theme'

interface Props {
  connected: boolean
  isDarkMode: boolean
  onToggleTheme: () => void
  theme: ThemePref
  onThemeChange: (t: ThemePref) => void
  telemetryEnabled: boolean
  onTelemetryChange: (enabled: boolean) => void
  query: string
  onQueryChange: (q: string) => void
  currentUser: string | null
}

const THEME_OPTIONS: { value: ThemePref; label: string }[] = [
  { value: 'system', label: 'System' },
  { value: 'light', label: 'Light' },
  { value: 'dark', label: 'Dark' },
]


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
  query,
  onQueryChange,
  currentUser,
}: Props) {
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [menuOpen, setMenuOpen] = useState(false)
  const searchRef = useRef<HTMLInputElement>(null)
  const menuRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!menuOpen) return
    const onClick = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setMenuOpen(false)
      }
    }
    const onEsc = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setMenuOpen(false)
    }
    document.addEventListener('mousedown', onClick)
    document.addEventListener('keydown', onEsc)
    return () => {
      document.removeEventListener('mousedown', onClick)
      document.removeEventListener('keydown', onEsc)
    }
  }, [menuOpen])

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
        <div className="flex items-center justify-between h-14 gap-3">
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

          <div className="flex items-center gap-2 min-w-0">
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

            <div ref={menuRef} className="relative">
              <button
                type="button"
                onClick={() => setMenuOpen((o) => !o)}
                title={currentUser ?? undefined}
                className="inline-flex items-center gap-1.5 rounded-full border border-border bg-muted/50 hover:bg-muted px-2.5 h-7 max-w-[240px] cursor-pointer transition-colors"
              >
                <span
                  className={cn(
                    'h-1.5 w-1.5 rounded-full shrink-0',
                    connected ? 'bg-green-500' : 'bg-red-500'
                  )}
                  aria-label={connected ? strings.app.connected : strings.app.disconnected}
                />
                <User className="h-3 w-3 shrink-0 text-muted-foreground" />
                <span className="font-mono text-meta text-foreground truncate">
                  {currentUser ?? '…'}
                </span>
                <ChevronDown className="h-3 w-3 shrink-0 text-muted-foreground" />
              </button>

              {menuOpen && (
                <div className="absolute right-0 top-full mt-1.5 z-50 min-w-[240px] rounded-lg border border-border bg-popover text-popover-foreground shadow-lg p-2 flex flex-col gap-2">
                  {currentUser && (
                    <div className="px-2 py-1 border-b border-border -mx-2 mb-1">
                      <div className="text-caps text-muted-foreground">Signed in as</div>
                      <div className="font-mono text-meta text-foreground truncate" title={currentUser}>
                        {currentUser}
                      </div>
                    </div>
                  )}

                  <div className="flex items-center justify-between px-2">
                    <span className="text-meta text-muted-foreground">Connection</span>
                    <span className="inline-flex items-center gap-1.5 text-meta">
                      <span
                        className={cn(
                          'h-1.5 w-1.5 rounded-full',
                          connected ? 'bg-green-500' : 'bg-red-500'
                        )}
                      />
                      {connected ? strings.app.connected : strings.app.disconnected}
                    </span>
                  </div>

                  <div className="px-2">
                    <div className="text-meta text-muted-foreground mb-1">Theme</div>
                    <div className="flex rounded-md border border-border overflow-hidden">
                      {THEME_OPTIONS.map((opt) => (
                        <button
                          key={opt.value}
                          type="button"
                          onClick={() => onThemeChange(opt.value)}
                          className={cn(
                            'flex-1 text-meta py-1.5 transition-colors',
                            theme === opt.value
                              ? 'bg-foreground text-background font-semibold'
                              : 'text-muted-foreground hover:bg-muted'
                          )}
                        >
                          {opt.label}
                        </button>
                      ))}
                    </div>
                  </div>

                  <button
                    type="button"
                    onClick={() => {
                      setMenuOpen(false)
                      setSettingsOpen(true)
                    }}
                    className="flex items-center gap-2 px-2 py-1.5 rounded text-meta text-foreground hover:bg-muted transition-colors"
                  >
                    <Settings className="h-3.5 w-3.5 text-muted-foreground" />
                    {strings.app.settings}
                  </button>
                </div>
              )}
            </div>
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
