import { Button, cn } from '@metalbear/ui'
import type { EventTypeValue } from '../../eventTypes'
import { FILTER_CHIPS } from './eventConfig'

interface Props {
  activeFilter: EventTypeValue | null
  counts: Partial<Record<string, number>>
  onChange: (filter: EventTypeValue | null) => void
}

export default function EventFilterChips({ activeFilter, counts, onChange }: Props) {
  return (
    <div className="flex items-center gap-0.5 bg-muted/30 border border-border rounded-lg p-0.5">
      {FILTER_CHIPS.map((chip) => {
        const active = activeFilter === chip.type
        const count = chip.type === null ? undefined : (counts[chip.type] ?? 0)
        return (
          <Button
            key={chip.label}
            variant="ghost"
            size="sm"
            style={{ fontSize: 11 }}
            onClick={() => onChange(chip.type)}
            className={cn(
              'font-medium px-2 py-0 h-5 leading-none rounded-md transition-colors gap-1',
              active
                ? 'bg-foreground text-background hover:bg-foreground hover:text-background'
                : 'text-muted-foreground hover:text-foreground'
            )}
          >
            {chip.label}
            {count !== undefined && count > 0 && (
              <span className={cn('tabular-nums', active ? 'text-background/70' : 'text-muted-foreground/60')}>
                {count}
              </span>
            )}
          </Button>
        )
      })}
    </div>
  )
}
