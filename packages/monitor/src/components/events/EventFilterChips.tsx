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
          className={cn(
            'text-[10px] font-medium px-2 py-0.5 h-auto rounded-full border',
            activeFilter === chip.type ? chip.activeClass : chip.colorClass
          )}
        >
          {chip.label}
        </Button>
      ))}
    </div>
  )
}
