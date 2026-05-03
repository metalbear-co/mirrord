import { Button, cn } from '@metalbear/ui'
import type { EventTypeValue } from '../../eventTypes'
import { FILTER_CHIPS } from './eventConfig'

interface Props {
  activeFilter: EventTypeValue | null
  onChange: (filter: EventTypeValue | null) => void
}

export default function EventFilterChips({ activeFilter, onChange }: Props) {
  return (
    <div className="flex items-center gap-1">
      {FILTER_CHIPS.map((chip) => (
        <Button
          key={chip.label}
          variant="ghost"
          size="sm"
          onClick={() => onChange(chip.type)}
          style={{ fontSize: 10 }}
          className={cn(
            'font-medium px-2 py-0 h-5 leading-none rounded-full border transition-colors',
            activeFilter === chip.type ? chip.activeClass : chip.colorClass
          )}
        >
          {chip.label}
        </Button>
      ))}
    </div>
  )
}
