import { cn } from '@metalbear/ui'
import { trackEvent } from '../analytics'
import type { DetailTab, TabDef } from './sessionDetailTypes'

interface Props {
  tabs: TabDef[]
  activeTab: DetailTab
  onTabChange: (tab: DetailTab) => void
}

export default function SessionTabs({ tabs, activeTab, onTabChange }: Props) {
  return (
    <div className="flex border-b border-border bg-card/20 shrink-0">
      {tabs.map((tab) => {
        const Icon = tab.icon
        return (
          <button
            key={tab.id}
            onClick={() => {
              trackEvent('session_monitor_tab_switch', { tab: tab.id })
              onTabChange(tab.id)
            }}
            className={cn(
              'flex items-center gap-1.5 px-4 py-2 text-xs font-medium border-b-2 transition-colors',
              activeTab === tab.id
                ? 'text-foreground border-primary'
                : 'text-muted-foreground border-transparent hover:text-foreground hover:bg-muted/30'
            )}
          >
            <Icon className="h-3 w-3" />
            {tab.label}
            {tab.count !== undefined && tab.count > 0 && (
              <span className="text-[9px] font-mono tabular-nums text-muted-foreground ml-0.5">
                {tab.count > 999 ? `${(tab.count / 1000).toFixed(1)}k` : tab.count}
              </span>
            )}
          </button>
        )
      })}
    </div>
  )
}
