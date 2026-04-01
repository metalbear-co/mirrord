import { useState, useEffect, useRef } from 'react'
import { Badge, Separator, cn } from '@metalbear/ui'
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@metalbear/ui'
import { FileText, Globe, Zap, Activity, Server, Info, Settings, Search, X, Trash2, ExternalLink } from 'lucide-react'
import type { SessionInfo, MonitorEvent } from './types'

const EVENT_TYPE_CONFIG: Record<string, {
  variant: 'default' | 'destructive' | 'outline' | 'secondary'
  label: string
  className: string
  icon: typeof Activity
  chipColor: string
}> = {
  file_op: { variant: 'outline', label: 'File', className: 'border-amber-500/50 text-amber-400 bg-amber-500/10', icon: FileText, chipColor: 'amber' },
  dns_query: { variant: 'outline', label: 'DNS', className: 'border-blue-500/50 text-blue-400 bg-blue-500/10', icon: Globe, chipColor: 'blue' },
  incoming_request: { variant: 'outline', label: 'HTTP', className: 'border-green-500/50 text-green-400 bg-green-500/10', icon: Zap, chipColor: 'green' },
  outgoing_connection: { variant: 'outline', label: 'Out', className: 'border-purple-500/50 text-purple-400 bg-purple-500/10', icon: Server, chipColor: 'purple' },
  port_subscription: { variant: 'outline', label: 'Port', className: 'border-indigo-500/50 text-indigo-400 bg-indigo-500/10', icon: Server, chipColor: 'indigo' },
  env_var: { variant: 'outline', label: 'Env', className: 'border-pink-500/50 text-pink-400 bg-pink-500/10', icon: Settings, chipColor: 'pink' },
  layer_connected: { variant: 'outline', label: 'Info', className: 'border-sky-500/50 text-sky-400 bg-sky-500/10', icon: Info, chipColor: 'sky' },
  layer_disconnected: { variant: 'outline', label: 'Info', className: 'border-sky-500/50 text-sky-400 bg-sky-500/10', icon: Info, chipColor: 'sky' },
}

const FILTER_CHIPS: { type: string | null; label: string; colorClass: string; activeClass: string }[] = [
  { type: null, label: 'All', colorClass: 'border-primary/40 text-primary/70 hover:text-primary', activeClass: 'border-primary bg-primary/15 text-primary' },
  { type: 'incoming_request', label: 'HTTP', colorClass: 'border-green-500/30 text-green-500/60 hover:text-green-400', activeClass: 'border-green-500 bg-green-500/15 text-green-400' },
  { type: 'dns_query', label: 'DNS', colorClass: 'border-blue-500/30 text-blue-500/60 hover:text-blue-400', activeClass: 'border-blue-500 bg-blue-500/15 text-blue-400' },
  { type: 'file_op', label: 'File', colorClass: 'border-amber-500/30 text-amber-500/60 hover:text-amber-400', activeClass: 'border-amber-500 bg-amber-500/15 text-amber-400' },
  { type: 'outgoing_connection', label: 'Out', colorClass: 'border-purple-500/30 text-purple-500/60 hover:text-purple-400', activeClass: 'border-purple-500 bg-purple-500/15 text-purple-400' },
  { type: 'port_subscription', label: 'Port', colorClass: 'border-indigo-500/30 text-indigo-500/60 hover:text-indigo-400', activeClass: 'border-indigo-500 bg-indigo-500/15 text-indigo-400' },
  { type: 'env_var', label: 'Env', colorClass: 'border-pink-500/30 text-pink-500/60 hover:text-pink-400', activeClass: 'border-pink-500 bg-pink-500/15 text-pink-400' },
]

const MAX_EVENTS = 500

interface ParsedEvent {
  type: string
  summary: string
  rawData?: string
}

function parseEvent(event: MonitorEvent): ParsedEvent {
  switch (event.type) {
    case 'file_op':
      return { type: 'file_op', summary: `${event.operation}: ${event.path || '?'}` }
    case 'dns_query':
      return { type: 'dns_query', summary: `DNS lookup: ${event.host}` }
    case 'incoming_request':
      return { type: 'incoming_request', summary: `${event.method} ${event.host}${event.path}` }
    case 'outgoing_connection':
      return { type: 'outgoing_connection', summary: `Outgoing: ${event.address}:${event.port}` }
    case 'port_subscription':
      return { type: 'port_subscription', summary: `Port ${event.port} subscribed (${event.mode})` }
    case 'env_var':
      return { type: 'env_var', summary: `Fetched ${event.vars.length} env vars` }
    case 'layer_connected':
      return { type: 'layer_connected', summary: `Process connected: ${event.process_name} (PID ${event.pid})` }
    case 'layer_disconnected':
      return { type: 'layer_disconnected', summary: `Process disconnected (PID ${event.pid})` }
  }
}

function formatTime24(date: Date): string {
  return date.toLocaleTimeString('en-GB', { hour12: false })
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
  const [activeFilter, setActiveFilter] = useState<string | null>(null)
  const logRef = useRef<HTMLDivElement>(null)
  const searchRef = useRef<HTMLInputElement>(null)
  const [showSearch, setShowSearch] = useState(false)

  useEffect(() => {
    setEvents([])
    setStreaming(true)

    const eventSource = new EventSource(`/api/sessions/${session.session_id}/events`)

    eventSource.onmessage = (e) => {
      let event: MonitorEvent
      try {
        event = JSON.parse(e.data)
      } catch {
        return
      }
      setEvents((prev) => {
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
    const el = logRef.current
    if (!el) return
    const handleScroll = () => {
      isNearBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 50
    }
    el.addEventListener('scroll', handleScroll)
    return () => el.removeEventListener('scroll', handleScroll)
  }, [])

  useEffect(() => {
    if (logRef.current && isNearBottom.current) {
      logRef.current.scrollTop = logRef.current.scrollHeight
    }
  }, [events])

  const processedEvents = events.map(({ event, receivedAt }) => ({
    event,
    receivedAt,
    parsed: parseEvent(event),
  }))

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
                placeholder="Filter events..."
                className="flex-1 bg-transparent text-xs text-foreground placeholder:text-muted-foreground/50 outline-none border border-border rounded px-2 py-1 focus:border-primary"
              />
              <button
                onClick={() => { setShowSearch(false); setSearchQuery('') }}
                className="text-muted-foreground hover:text-foreground"
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>
          ) : (
            <button
              onClick={() => { setShowSearch(true); setTimeout(() => searchRef.current?.focus(), 100) }}
              className="p-1 rounded hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
              title="Search events (Ctrl+F)"
            >
              <Search className="h-3.5 w-3.5" />
            </button>
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
          {filteredEvents.length}{activeFilter !== null || searchQuery ? `/${processedEvents.length}` : ''} events
        </span>

        <button
          onClick={() => setEvents([])}
          className="p-1 rounded hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
          title="Clear events"
        >
          <Trash2 className="h-3.5 w-3.5" />
        </button>
      </div>

      <div ref={logRef} className="flex-1 overflow-y-auto text-xs font-mono">
        {filteredEvents.length === 0 && streaming && (
          <div className="text-muted-foreground text-center py-8 flex flex-col items-center gap-2">
            <Activity className="h-6 w-6 opacity-30 animate-pulse" />
            <span>Waiting for events...</span>
          </div>
        )}
        {filteredEvents.map(({ parsed, receivedAt }, i) => {
          const config = EVENT_TYPE_CONFIG[parsed.type] ?? EVENT_TYPE_CONFIG['outgoing_connection']!
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
