import { cn } from '@metalbear/ui'

export type Tab = 'sessions' | 'events' | 'config'

interface TabBarProps {
  activeTab: Tab
  onTabChange: (tab: Tab) => void
}

const TABS: { id: Tab; label: string }[] = [
  { id: 'sessions', label: 'Sessions' },
  { id: 'events', label: 'Events' },
  { id: 'config', label: 'Config' },
]

export default function TabBar({ activeTab, onTabChange }: TabBarProps) {
  return (
    <div className="flex border-b border-border bg-card/30">
      {TABS.map((tab) => (
        <button
          key={tab.id}
          onClick={() => onTabChange(tab.id)}
          className={cn(
            'px-5 py-2 text-xs font-medium border-b-2 transition-colors',
            activeTab === tab.id
              ? 'text-foreground border-primary bg-primary/5'
              : 'text-muted-foreground border-transparent hover:text-foreground hover:bg-muted/30'
          )}
        >
          {tab.label}
        </button>
      ))}
    </div>
  )
}
