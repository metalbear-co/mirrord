import { Button, cn } from '@metalbear/ui'
import { Zap } from 'lucide-react'
import type { EventTypeValue } from '../../eventTypes'
import { strings } from '../../strings'
import { FILTER_CHIPS, CHIP_INACTIVE } from './eventConfig'

interface Props {
  activeFilter: EventTypeValue | null
  onChange: (filter: EventTypeValue | null) => void
  affectedActive?: boolean
  onAffectedChange?: ((active: boolean) => void) | undefined
}

export default function EventFilterChips({
  activeFilter,
  onChange,
  affectedActive = false,
  onAffectedChange,
}: Props) {
  return (
    <div className="flex items-center gap-1">
      {FILTER_CHIPS.map((chip) => (
        <Button
          key={chip.label}
          variant="ghost"
          size="sm"
          onClick={() => onChange(chip.type)}
          className={cn(
            'text-caps h-5 rounded-full border px-2 py-0 font-medium leading-none transition-colors',
            !affectedActive && activeFilter === chip.type
              ? chip.activeClass
              : chip.colorClass,
          )}
        >
          {chip.label}
        </Button>
      ))}
      {onAffectedChange && (
        <Button
          variant="ghost"
          size="sm"
          onClick={() => onAffectedChange(!affectedActive)}
          className={cn(
            'text-caps h-5 gap-1 rounded-full border px-2 py-0 font-medium leading-none transition-colors',
            affectedActive
              ? 'border-chaos/60 bg-chaos/15 text-chaos hover:text-chaos'
              : CHIP_INACTIVE,
          )}
        >
          <Zap className="h-2.5 w-2.5" />
          {strings.chaos.affectedFilter}
        </Button>
      )}
    </div>
  )
}
