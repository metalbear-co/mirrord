import { Button, cn } from '@metalbear/ui'
import type { EventTypeValue } from '../../eventTypes'
import { strings } from '../../strings'
import { FILTER_CHIPS } from './eventConfig'

// The type chips plus a status-based errors chip: when debugging, "just the failures" is the
// filter people reach for first.
export type EventFilter = EventTypeValue | 'errors' | null

interface Props {
  activeFilter: EventFilter
  counts: Partial<Record<string, number>>
  errorCount: number
  onChange: (filter: EventFilter) => void
}

export default function EventFilterChips({ activeFilter, counts, errorCount, onChange }: Props) {
  const chips: { type: EventFilter; label: string; count?: number; error?: boolean }[] = [
    ...FILTER_CHIPS.map((chip) => ({
      type: chip.type as EventFilter,
      label: chip.label,
      count: chip.type === null ? undefined : (counts[chip.type] ?? 0),
    })),
    { type: 'errors', label: strings.events.errors, count: errorCount, error: true },
  ]

  return (
    <div className="flex items-center gap-0.5 bg-muted/30 border border-border rounded-lg p-0.5">
      {chips.map((chip) => {
        const active = activeFilter === chip.type
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
            {chip.count !== undefined && chip.count > 0 && (
              <span
                className={cn(
                  'tabular-nums',
                  active
                    ? 'text-background/70'
                    : chip.error
                      ? 'text-red-600 dark:text-red-400 font-semibold'
                      : 'text-muted-foreground/60'
                )}
              >
                {chip.count}
              </span>
            )}
          </Button>
        )
      })}
    </div>
  )
}
