import { Button, cn } from '@metalbear/ui'
import { trackEvent } from '../analytics'
import type { DetailTab, TabDef } from './sessionDetailTypes'

interface Props {
  tabs: TabDef[]
  activeTab: DetailTab
  onTabChange: (tab: DetailTab) => void
}

export default function SessionTabs({ tabs, activeTab, onTabChange }: Props) {
  return (
    <div className="flex border-b border-border surface-inset shrink-0">
      {tabs.map((tab) => {
        const Icon = tab.icon
        const isActive = activeTab === tab.id
        return (
          <Button
            key={tab.id}
            variant="ghost"
            size="sm"
            onClick={() => {
              trackEvent('session_monitor_tab_switch', { tab: tab.id })
              onTabChange(tab.id)
            }}
            className={cn(
              'rounded-none h-auto px-4 py-2 text-xs font-medium gap-1.5 border-b-2',
              isActive
                ? 'text-foreground border-primary hover:bg-transparent'
                : 'text-muted-foreground border-transparent hover:text-foreground hover:bg-muted/30'
            )}
          >
            <Icon className="h-3 w-3" />
            {tab.label}
            {tab.count !== undefined && tab.count > 0 && (
              <span className="text-caps font-mono tabular-nums text-muted-foreground ml-0.5">
                {tab.count > 999 ? `${(tab.count / 1000).toFixed(1)}k` : tab.count}
              </span>
            )}
          </Button>
        )
      })}
    </div>
  )
}
