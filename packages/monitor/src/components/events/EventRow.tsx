import { Badge, cn } from '@metalbear/ui'
import { ExternalLink, Zap } from 'lucide-react'
import { strings } from '../../strings'
import { EVENT_TYPE_CONFIG, DEFAULT_EVENT_CONFIG } from './eventConfig'
import type { ParsedEvent } from './parseEvent'
import { formatTime24 } from './parseEvent'

export interface EventChaosTag {
  name: string
  isError: boolean
}

interface Props {
  parsed: ParsedEvent
  receivedAt: Date
  zebra: boolean
  onClick?: (() => void) | undefined
  chaosTag?: EventChaosTag | null | undefined
  onBreak?: (() => void) | null | undefined
}

export default function EventRow({
  parsed,
  receivedAt,
  zebra,
  onClick,
  chaosTag,
  onBreak,
}: Props) {
  const config = EVENT_TYPE_CONFIG[parsed.type] ?? DEFAULT_EVENT_CONFIG
  const time = formatTime24(receivedAt)
  const Icon = config.icon
  const hasDetail = !!parsed.rawData

  return (
    <div
      className={cn(
        'event-row-animate group flex items-center gap-2 px-3 py-[3px] transition-colors',
        hasDetail ? 'hover:bg-muted/50 cursor-pointer' : 'hover:bg-muted/30',
        zebra && 'bg-muted/5',
        chaosTag && (chaosTag.isError ? 'bg-destructive/10' : 'bg-chaos/10'),
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
          chaosTag?.isError && 'text-destructive',
        )}
      >
        {parsed.summary}
      </span>
      {chaosTag && (
        <span className="text-chaos text-meta flex shrink-0 items-center gap-1 font-sans">
          <Zap className="h-2.5 w-2.5" />
          {chaosTag.name}
        </span>
      )}
      {!chaosTag && onBreak && (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation()
            onBreak()
          }}
          className="border-border text-muted-foreground hover:border-primary hover:bg-primary focus-visible:ring-ring flex shrink-0 items-center gap-1 rounded-md border px-2 py-px font-sans font-semibold opacity-0 transition-opacity hover:text-white focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-1 group-hover:opacity-100"
          style={{ fontSize: 10.5 }}
        >
          <Zap className="h-2.5 w-2.5" />
          {strings.chaos.breakThis}
        </button>
      )}
      {hasDetail && (
        <ExternalLink className="text-primary/60 h-3 w-3 shrink-0" />
      )}
    </div>
  )
}
