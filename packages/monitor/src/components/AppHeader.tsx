import { Button, MirrordIcon, cn } from '@metalbear/ui'
import { Sun, Moon } from 'lucide-react'
import { strings } from '../strings'

interface Props {
  connected: boolean
  isDarkMode: boolean
  onToggleTheme: () => void
}

export default function AppHeader({ connected, isDarkMode, onToggleTheme }: Props) {
  return (
    <header className="relative shrink-0 bg-background border-b border-border text-foreground">
      <div className="absolute bottom-0 left-0 right-0 h-[2px] bg-gradient-to-r from-transparent via-primary to-transparent opacity-40" />
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
          </div>
        </div>
      </div>
    </header>
  )
}
