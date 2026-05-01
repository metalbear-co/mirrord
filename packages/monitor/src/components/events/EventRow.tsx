import { Badge, cn } from '@metalbear/ui'
import { ExternalLink } from 'lucide-react'
import { EventType } from '../../eventTypes'
import { EVENT_TYPE_CONFIG } from './eventConfig'
import type { ParsedEvent } from './parseEvent'
import { formatTime24 } from './parseEvent'

interface Props {
  parsed: ParsedEvent
  receivedAt: Date
  zebra: boolean
  onClick?: () => void
}

export default function EventRow({ parsed, receivedAt, zebra, onClick }: Props) {
  const config = EVENT_TYPE_CONFIG[parsed.type] ?? EVENT_TYPE_CONFIG[EventType.OutgoingConnection]!
  const time = formatTime24(receivedAt)
  const Icon = config.icon
  const hasDetail = !!parsed.rawData

  return (
    <div
      className={cn(
        'flex items-center gap-2 py-[3px] px-3 transition-colors event-row-animate',
        hasDetail ? 'hover:bg-muted/50 cursor-pointer' : 'hover:bg-muted/30',
        zebra && 'bg-muted/5'
      )}
      onClick={hasDetail ? onClick : undefined}
    >
      <span className="text-muted-foreground text-caps shrink-0 w-[60px] tabular-nums">
        {time}
      </span>
      <Badge
        variant={config.variant}
        className={cn('shrink-0 w-[44px] justify-center text-caps font-semibold px-1 py-0 gap-0.5', config.className)}
      >
        <Icon className="h-2.5 w-2.5" />
        {config.label}
      </Badge>
      <span className={cn(
        'leading-snug flex-1 text-foreground/90 truncate',
        hasDetail && 'underline decoration-dotted decoration-muted-foreground/30 underline-offset-2'
      )}>
        {parsed.summary}
      </span>
      {hasDetail && (
        <ExternalLink className="h-3 w-3 text-primary/60 shrink-0" />
      )}
    </div>
  )
}
