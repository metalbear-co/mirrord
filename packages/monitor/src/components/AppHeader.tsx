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
    <header className="dark:bg-dark-background bg-white/80 backdrop-blur-sm border-b border-border dark:border-transparent shrink-0">
      <div className="px-6">
        <div className="flex items-center justify-between h-12 text-foreground">
          <div className="flex items-center gap-3">
            <img src={MirrordIcon} alt="mirrord" className="w-7 h-7 dark:invert" />
            <span className="font-semibold text-base">{strings.app.title}</span>
            <span className="opacity-30">|</span>
            <span className="text-sm font-medium opacity-80">{strings.app.subtitle}</span>
          </div>
          <div className="flex items-center gap-3">
            <div className="flex items-center gap-2">
              <div
                className={cn('h-2 w-2 rounded-full', connected ? 'bg-green-500' : 'bg-red-500')}
              />
              <span className="text-xs opacity-60">
                {connected ? strings.app.connected : strings.app.disconnected}
              </span>
            </div>
            <span className="opacity-20">|</span>
            <Button
              variant="ghost"
              size="icon"
              onClick={onToggleTheme}
              title={isDarkMode ? strings.app.themeLight : strings.app.themeDark}
              className="h-7 w-7 opacity-60 hover:opacity-100"
            >
              {isDarkMode ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
            </Button>
          </div>
        </div>
      </div>
    </header>
  )
}
