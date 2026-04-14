import { useState, useEffect, useRef } from 'react'
import { Badge, Button, Separator, cn } from '@metalbear/ui'
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@metalbear/ui'
import { FileText, Globe, Zap, Activity, Server, Info, Search, X, Trash2, ExternalLink } from 'lucide-react'
import type { SessionInfo, MonitorEvent } from '../types'
import { EventType, type EventTypeValue } from '../eventTypes'
import { strings } from '../strings'
import { api } from '../api'

// Badge colors use distinct per-category tokens (amber/blue/green/purple/sky) so
// event kinds remain visually distinguishable. The UI kit's semantic Badge variants
// only cover default/destructive/secondary/outline, which isn't enough categories.
const EVENT_TYPE_CONFIG: Record<EventTypeValue, {
  variant: 'default' | 'destructive' | 'outline' | 'secondary'
  label: string
  className: string
  icon: typeof Activity
} | undefined> = {
  [EventType.FileOp]: { variant: 'outline', label: strings.events.labels.file, className: 'border-amber-500/50 text-amber-400 bg-amber-500/10', icon: FileText },
  [EventType.DnsQuery]: { variant: 'outline', label: strings.events.labels.dns, className: 'border-blue-500/50 text-blue-400 bg-blue-500/10', icon: Globe },
  [EventType.IncomingRequest]: { variant: 'outline', label: strings.events.labels.http, className: 'border-green-500/50 text-green-400 bg-green-500/10', icon: Zap },
  [EventType.OutgoingConnection]: { variant: 'outline', label: strings.events.labels.out, className: 'border-purple-500/50 text-purple-400 bg-purple-500/10', icon: Server },
  [EventType.LayerConnected]: { variant: 'outline', label: strings.events.labels.info, className: 'border-sky-500/50 text-sky-400 bg-sky-500/10', icon: Info },
  [EventType.LayerDisconnected]: { variant: 'outline', label: strings.events.labels.info, className: 'border-sky-500/50 text-sky-400 bg-sky-500/10', icon: Info },
  [EventType.PortSubscription]: undefined,
  [EventType.EnvVar]: undefined,
}

const FILTER_CHIPS: { type: EventTypeValue | null; label: string; colorClass: string; activeClass: string }[] = [
  { type: null, label: strings.events.all, colorClass: 'border-primary/40 text-primary/70 hover:text-primary', activeClass: 'border-primary bg-primary/15 text-primary' },
  { type: EventType.IncomingRequest, label: strings.events.incoming, colorClass: 'border-green-500/30 text-green-500/60 hover:text-green-400', activeClass: 'border-green-500 bg-green-500/15 text-green-400' },
  { type: EventType.DnsQuery, label: strings.events.dns, colorClass: 'border-blue-500/30 text-blue-500/60 hover:text-blue-400', activeClass: 'border-blue-500 bg-blue-500/15 text-blue-400' },
  { type: EventType.FileOp, label: strings.events.fileOps, colorClass: 'border-amber-500/30 text-amber-500/60 hover:text-amber-400', activeClass: 'border-amber-500 bg-amber-500/15 text-amber-400' },
  { type: EventType.OutgoingConnection, label: strings.events.outgoing, colorClass: 'border-purple-500/30 text-purple-500/60 hover:text-purple-400', activeClass: 'border-purple-500 bg-purple-500/15 text-purple-400' },
]

const MAX_EVENTS = 500

interface ParsedEvent {
  type: EventTypeValue
  summary: string
  rawData?: string
}

// Returns null for event kinds that should not be shown in the event log
// (port_subscription, env_var — surfaced elsewhere in the UI) or for malformed events.
function parseEvent(event: MonitorEvent): ParsedEvent | null {
  try {
    switch (event.type) {
      case EventType.FileOp:
        return { type: EventType.FileOp, summary: `${event.operation}: ${event.path || strings.events.unknownPath}` }
      case EventType.DnsQuery:
        return { type: EventType.DnsQuery, summary: `DNS lookup: ${event.host}` }
      case EventType.IncomingRequest:
        return { type: EventType.IncomingRequest, summary: `${event.method} ${event.host}${event.path}` }
      case EventType.OutgoingConnection:
        return { type: EventType.OutgoingConnection, summary: `Outgoing: ${event.address}:${event.port}` }
      case EventType.PortSubscription:
      case EventType.EnvVar:
        return null
      case EventType.LayerConnected:
        return { type: EventType.LayerConnected, summary: `Process connected: ${event.process_name} (PID ${event.pid})` }
      case EventType.LayerDisconnected:
        return { type: EventType.LayerDisconnected, summary: `Process disconnected (PID ${event.pid})` }
      default:
        return null
    }
  } catch (err) {
    console.warn('Failed to parse event', event, err)
    return null
  }
}

function formatTime24(date: Date): string {
  // Pass undefined locale so the browser uses the user's default locale.
  return date.toLocaleTimeString(undefined, { hour12: false })
}

interface TimestampedEvent {
  event: MonitorEvent
  receivedAt: Date
}

interface Props {
  session: SessionInfo
}

export default function EventStream({ session }: Props) {
  const [events, setEvents] = useState<TimestampedEvent[]>([])
  const [streaming, setStreaming] = useState(false)
  const [detailEvent, setDetailEvent] = useState<{ summary: string; raw: string } | null>(null)
  const [searchQuery, setSearchQuery] = useState('')
  const [activeFilter, setActiveFilter] = useState<EventTypeValue | null>(null)
  const logRef = useRef<HTMLDivElement>(null)
  const searchRef = useRef<HTMLInputElement>(null)
  const [showSearch, setShowSearch] = useState(false)

  useEffect(() => {
    setEvents([])
    setStreaming(true)

    const eventSource = new EventSource(api.eventStreamUrl(session.session_id))

    eventSource.onmessage = (e) => {
      let event: MonitorEvent
      try {
        event = JSON.parse(e.data)
      } catch {
        return
      }
      setEvents((prev) => {
        // Append the new event and cap the buffer at MAX_EVENTS by dropping
        // the oldest entries. Keeps memory bounded for long-running sessions.
        const next = [...prev, { event, receivedAt: new Date() }]
        return next.length > MAX_EVENTS ? next.slice(-MAX_EVENTS) : next
      })
    }

    eventSource.onerror = () => {
      setStreaming(false)
      eventSource.close()
    }

    return () => {
      eventSource.close()
      setStreaming(false)
    }
  }, [session.session_id])

  const isNearBottom = useRef(true)
  useEffect(() => {
    const logEl = logRef.current
    if (!logEl) return
    const handleScroll = () => {
      isNearBottom.current = logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight < 50
    }
    logEl.addEventListener('scroll', handleScroll)
    return () => logEl.removeEventListener('scroll', handleScroll)
  }, [])

  useEffect(() => {
    if (logRef.current && isNearBottom.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight
    }
  }, [events])

  // parseEvent returns null for events that aren't displayed in the stream
  // (port_subscription/env_var are shown in Overview, plus malformed events).
  const processedEvents = events
    .map(({ event, receivedAt }) => ({
      event,
      receivedAt,
      parsed: parseEvent(event),
    }))
    .filter((e): e is { event: MonitorEvent; receivedAt: Date; parsed: ParsedEvent } => e.parsed !== null)

  const filteredEvents = processedEvents.filter(({ parsed }) => {
    const matchesType = activeFilter === null || parsed.type === activeFilter
    const matchesSearch = !searchQuery || parsed.summary.toLowerCase().includes(searchQuery.toLowerCase())
    return matchesType && matchesSearch
  })

  return (
    <div className="h-full flex flex-col">
      <div className="border-b border-border px-4 py-2 bg-card/30 flex items-center gap-3">
        <div className="flex items-center gap-2 flex-1">
          {showSearch ? (
            <div className="flex items-center gap-2 flex-1">
              <Search className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
              <input
                ref={searchRef}
                type="text"
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                placeholder={strings.events.searchPlaceholder}
                className="flex-1 bg-transparent text-xs text-foreground placeholder:text-muted-foreground/50 outline-none border border-border rounded px-2 py-1 focus:border-primary"
              />
              <Button
                variant="ghost"
                size="icon"
                onClick={() => { setShowSearch(false); setSearchQuery('') }}
                className="h-6 w-6"
              >
                <X className="h-3.5 w-3.5" />
              </Button>
            </div>
          ) : (
            <Button
              variant="ghost"
              size="icon"
              onClick={() => { setShowSearch(true); setTimeout(() => searchRef.current?.focus(), 100) }}
              title={strings.events.search}
              className="h-6 w-6"
            >
              <Search className="h-3.5 w-3.5" />
            </Button>
          )}
        </div>

        <div className="flex items-center gap-1">
          {FILTER_CHIPS.map((chip) => (
            <button
              key={chip.label}
              onClick={() => setActiveFilter(chip.type)}
              className={cn(
                'text-[10px] font-medium px-2 py-0.5 rounded-full border transition-colors',
                activeFilter === chip.type ? chip.activeClass : chip.colorClass
              )}
            >
              {chip.label}
            </button>
          ))}
        </div>

        <Separator orientation="vertical" className="h-3" />

        <span className="text-[10px] text-muted-foreground tabular-nums">
          {filteredEvents.length}{activeFilter !== null || searchQuery ? `/${processedEvents.length}` : ''} {strings.events.countSuffix}
        </span>

        <Button
          variant="ghost"
          size="icon"
          onClick={() => setEvents([])}
          title={strings.events.clear}
          className="h-6 w-6"
        >
          <Trash2 className="h-3.5 w-3.5" />
        </Button>
      </div>

      <div ref={logRef} className="flex-1 overflow-y-auto text-xs font-mono">
        {filteredEvents.length === 0 && streaming && (
          <div className="text-muted-foreground text-center py-8 flex flex-col items-center gap-2">
            <Activity className="h-6 w-6 opacity-30 animate-pulse" />
            <span>{strings.events.waiting}</span>
          </div>
        )}
        {filteredEvents.map(({ parsed, receivedAt }, i) => {
          const config = EVENT_TYPE_CONFIG[parsed.type] ?? EVENT_TYPE_CONFIG[EventType.OutgoingConnection]!
          const time = formatTime24(receivedAt)
          const Icon = config.icon
          const hasDetail = !!parsed.rawData

          return (
            <div
              key={`${receivedAt.getTime()}-${i}`}
              className={cn(
                'flex items-center gap-2 py-[3px] px-3 transition-colors event-row-animate',
                hasDetail ? 'hover:bg-muted/50 cursor-pointer' : 'hover:bg-muted/30',
                i % 2 === 0 ? 'bg-muted/5' : ''
              )}
              onClick={hasDetail ? () => setDetailEvent({ summary: parsed.summary, raw: parsed.rawData! }) : undefined}
            >
              <span className="text-muted-foreground text-[10px] shrink-0 w-[60px] tabular-nums">
                {time}
              </span>
              <Badge
                variant={config.variant}
                className={cn('shrink-0 w-[44px] justify-center text-[9px] font-semibold px-1 py-0 gap-0.5', config.className)}
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
        })}
      </div>

      <Dialog open={!!detailEvent} onOpenChange={() => setDetailEvent(null)}>
        <DialogContent className="max-w-4xl max-h-[85vh]">
          <DialogHeader>
            <DialogTitle className="text-sm font-medium text-foreground">
              {detailEvent?.summary}
            </DialogTitle>
          </DialogHeader>
          <div className="relative overflow-auto max-h-[70vh] rounded-lg border border-border bg-muted">
            <pre className="p-4 text-xs font-mono text-foreground whitespace-pre-wrap break-all">
              {detailEvent?.raw}
            </pre>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  )
}
