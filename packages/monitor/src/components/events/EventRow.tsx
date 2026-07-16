import { Badge, cn } from '@metalbear/ui'
import { ExternalLink } from 'lucide-react'
import { EVENT_TYPE_CONFIG, DEFAULT_EVENT_CONFIG } from './eventConfig'
import type { ParsedEvent } from './parseEvent'
import { formatTime24 } from './parseEvent'

interface Props {
  parsed: ParsedEvent
  receivedAt: Date
  zebra: boolean
  onClick?: (() => void) | undefined
}

export default function EventRow({
  parsed,
  receivedAt,
  zebra,
  onClick,
}: Props) {
  const config = EVENT_TYPE_CONFIG[parsed.type] ?? DEFAULT_EVENT_CONFIG
  const time = formatTime24(receivedAt)
  const Icon = config.icon
  const hasDetail = !!parsed.rawData

  return (
    <div
      className={cn(
        'event-row-animate flex items-center gap-2 px-3 py-[3px] transition-colors',
        hasDetail ? 'hover:bg-muted/50 cursor-pointer' : 'hover:bg-muted/30',
        zebra && 'bg-muted/5',
      )}
      role={hasDetail ? 'button' : undefined}
      tabIndex={hasDetail ? 0 : undefined}
      onClick={hasDetail ? onClick : undefined}
      onKeyDown={
        hasDetail
          ? (e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault()
                onClick?.()
              }
            }
          : undefined
      }
    >
      <span className="text-muted-foreground text-caps w-[60px] shrink-0 tabular-nums">
        {time}
      </span>
      <Badge
        variant={config.variant}
        style={{ fontSize: 10 }}
        className={cn(
          'h-4 w-[44px] shrink-0 justify-center gap-0.5 px-1 py-0 font-semibold',
          config.className,
        )}
      >
        <Icon className="h-2.5 w-2.5" />
        {config.label}
      </Badge>
      <span
        className={cn(
          'text-foreground/90 flex-1 truncate leading-snug',
          hasDetail &&
            'decoration-muted-foreground/30 underline decoration-dotted underline-offset-2',
        )}
      >
        {parsed.summary}
      </span>
      {hasDetail && (
        <ExternalLink className="text-primary/60 h-3 w-3 shrink-0" />
      )}
    </div>
  )
}
