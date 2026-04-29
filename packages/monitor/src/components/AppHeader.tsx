import { Button, MirrordIcon, cn } from '@metalbear/ui'
import { Moon, Settings, Sun } from 'lucide-react'
import { useState } from 'react'
import { strings } from '../strings'
import SettingsDialog from './SettingsDialog'

interface Props {
  connected: boolean
  isDarkMode: boolean
  onToggleTheme: () => void
  telemetryEnabled: boolean
  onTelemetryChange: (enabled: boolean) => void
}

export default function AppHeader({
  connected,
  isDarkMode,
  onToggleTheme,
  telemetryEnabled,
  onTelemetryChange,
}: Props) {
  const [settingsOpen, setSettingsOpen] = useState(false)

  return (
    <header className="relative shrink-0 bg-background border-b border-border text-foreground shadow-[0_1px_2px_-1px_rgb(0_0_0_/_0.04)]">
      {/* Hair-thin inner top highlight — a soft rim of light that gives the header a
          glassy finish in both themes, replacing the prior brand-colored accent line. */}
      <div className="pointer-events-none absolute inset-x-0 top-0 h-px bg-gradient-to-r from-transparent via-foreground/10 to-transparent" />
      <div className="px-4 sm:px-6 lg:px-8">
        <div className="flex items-center justify-between h-14">
          <div className="flex items-center gap-3">
            <img
              src={MirrordIcon}
              alt={strings.app.title}
              className={cn('w-8 h-8', isDarkMode && 'invert')}
            />
            <div className="hidden sm:flex items-center gap-2">
              <span className="font-semibold text-h4">{strings.app.title}</span>
              <span className="text-foreground/30">|</span>
              <span className="text-body-sm font-medium text-foreground/70">
                {strings.app.subtitle}
              </span>
            </div>
            <span className="font-semibold text-h4 sm:hidden">{strings.app.title}</span>
          </div>

          <div className="flex items-center gap-3">
            <div className="flex items-center gap-2">
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
        telemetryEnabled={telemetryEnabled}
        onTelemetryChange={onTelemetryChange}
      />
    </header>
  )
}
