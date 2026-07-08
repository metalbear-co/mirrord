import { Badge, cn } from '@metalbear/ui'
import { EventType } from '../../eventTypes'
import { EVENT_TYPE_CONFIG } from './eventConfig'
import type { ParsedEvent } from './parseEvent'
import { formatTime24 } from './parseEvent'

export const ROW_GRID = 'grid grid-cols-[72px_64px_68px_minmax(0,1fr)_60px_48px] items-center gap-2'

const STATUS_TONE = {
  ok: 'text-emerald-700 bg-emerald-100 dark:text-emerald-400 dark:bg-emerald-950',
  error: 'text-red-700 bg-red-100 dark:text-red-400 dark:bg-red-950',
  muted: 'text-muted-foreground bg-muted/40',
}

interface Props {
  parsed: ParsedEvent
  receivedAt: Date
  count: number
  selected: boolean
  onClick?: () => void
}

export default function EventRow({ parsed, receivedAt, count, selected, onClick }: Props) {
  const config = EVENT_TYPE_CONFIG[parsed.type] ?? EVENT_TYPE_CONFIG[EventType.OutgoingConnection]!
  const time = formatTime24(receivedAt)
  const hasDetail = parsed.rawData !== undefined

  return (
    <div
      className={cn(
        ROW_GRID,
        'py-[5px] px-3 border-b border-border/40 transition-colors event-row-animate border-l-[3px] border-l-transparent',
        hasDetail ? 'hover:bg-muted/50 cursor-pointer' : 'hover:bg-muted/30',
        selected && 'bg-primary/10 border-l-primary'
      )}
      onClick={hasDetail ? onClick : undefined}
    >
      <span className="text-muted-foreground font-mono text-[11px] tabular-nums">{time}</span>
      <span>
        <Badge
          variant={config.variant}
          style={{ fontSize: 10 }}
          className={cn('w-[44px] justify-center font-semibold px-1 py-0 gap-0.5 h-4', config.className)}
        >
          <config.icon className="h-2.5 w-2.5" />
          {config.label}
        </Badge>
      </span>
      <span className="font-mono text-xs font-bold truncate">{parsed.columns.method}</span>
      <span
        className={cn(
          'font-mono text-xs truncate text-foreground/90',
          hasDetail && 'underline decoration-dotted decoration-muted-foreground/30 underline-offset-2'
        )}
      >
        {parsed.columns.path}
      </span>
      <span>
        {parsed.columns.status && (
          <span
            className={cn(
              'text-[11px] font-semibold rounded-full px-2 py-px tabular-nums',
              STATUS_TONE[parsed.columns.statusTone]
            )}
          >
            {parsed.columns.status}
          </span>
        )}
      </span>
      <span className="text-right">
        {count > 1 && (
          <span className="text-[11px] text-muted-foreground bg-muted/40 border border-border rounded-full px-1.5 py-px tabular-nums">
            ×{count}
          </span>
        )}
      </span>
    </div>
  )
}
