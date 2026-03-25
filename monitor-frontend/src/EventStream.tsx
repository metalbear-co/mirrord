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
}> = {
  file_op: { variant: 'outline', label: 'File', className: 'border-amber-500/50 text-amber-400 bg-amber-500/10', icon: FileText },
  dns_query: { variant: 'outline', label: 'DNS', className: 'border-blue-500/50 text-blue-400 bg-blue-500/10', icon: Globe },
  incoming_request: { variant: 'outline', label: 'HTTP', className: 'border-green-500/50 text-green-400 bg-green-500/10', icon: Zap },
  outgoing_connection: { variant: 'outline', label: 'Out', className: 'border-purple-500/50 text-purple-400 bg-purple-500/10', icon: Server },
  port_subscription: { variant: 'outline', label: 'Port', className: 'border-indigo-500/50 text-indigo-400 bg-indigo-500/10', icon: Server },
  env_var: { variant: 'outline', label: 'Env', className: 'border-pink-500/50 text-pink-400 bg-pink-500/10', icon: Settings },
  layer_connected: { variant: 'outline', label: 'Info', className: 'border-sky-500/50 text-sky-400 bg-sky-500/10', icon: Info },
  layer_disconnected: { variant: 'outline', label: 'Info', className: 'border-sky-500/50 text-sky-400 bg-sky-500/10', icon: Info },
}

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
  const [showSearch, setShowSearch] = useState(false)
  const logRef = useRef<HTMLDivElement>(null)
  const searchRef = useRef<HTMLInputElement>(null)

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

  // Auto-scroll only if user is near the bottom
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

  const filteredEvents = searchQuery
    ? processedEvents.filter(({ parsed }) =>
        parsed.summary.toLowerCase().includes(searchQuery.toLowerCase())
      )
    : processedEvents

  return (
    <div className="h-full flex flex-col">
      <div className="border-b border-border px-6 py-3 bg-card/50">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <Server className="h-4 w-4 text-muted-foreground" />
            <span className="font-mono font-semibold text-foreground">
              {session.target}
            </span>
            <Separator orientation="vertical" className="h-4" />
            <span className="text-muted-foreground text-sm font-mono">
              {session.session_id}
            </span>
          </div>
          <div className="flex items-center gap-2">
            <div className="flex items-center gap-1.5">
              <div
                className={cn(
                  'h-2 w-2 rounded-full',
                  streaming ? 'bg-green-500 animate-pulse' : 'bg-destructive'
                )}
              />
              <span className="text-xs text-muted-foreground">
                {streaming ? 'Streaming' : 'Disconnected'}
              </span>
            </div>
            <Separator orientation="vertical" className="h-3" />
            <span className="text-xs text-muted-foreground">
              {filteredEvents.length} events
            </span>
            <Separator orientation="vertical" className="h-3" />
            <button
              onClick={() => {
                setShowSearch(!showSearch)
                if (!showSearch) setTimeout(() => searchRef.current?.focus(), 100)
                else setSearchQuery('')
              }}
              className={cn(
                'p-1 rounded hover:bg-muted transition-colors',
                showSearch ? 'text-primary' : 'text-muted-foreground hover:text-foreground'
              )}
              title="Search events"
            >
              <Search className="h-3.5 w-3.5" />
            </button>
            <button
              onClick={() => setEvents([])}
              className="p-1 rounded hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
              title="Clear events"
            >
              <Trash2 className="h-3.5 w-3.5" />
            </button>
          </div>
        </div>
      </div>

      {showSearch && (
        <div className="border-b border-border px-4 py-2 flex items-center gap-2 bg-card/30">
          <Search className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
          <input
            ref={searchRef}
            type="text"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            placeholder="Filter events..."
            className="flex-1 bg-transparent text-sm text-foreground placeholder:text-muted-foreground/50 outline-none"
          />
          {searchQuery && (
            <button onClick={() => setSearchQuery('')} className="text-muted-foreground hover:text-foreground">
              <X className="h-3.5 w-3.5" />
            </button>
          )}
          <span className="text-xs text-muted-foreground">
            {filteredEvents.length}/{processedEvents.length}
          </span>
        </div>
      )}

      <div ref={logRef} className="flex-1 overflow-y-auto p-4 text-sm">
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
                'flex items-start gap-2.5 py-1 px-3 rounded-md transition-colors event-row-animate',
                hasDetail ? 'hover:bg-muted/50 cursor-pointer' : 'hover:bg-muted/30',
                i % 2 === 0 ? 'bg-muted/10' : ''
              )}
              onClick={hasDetail ? () => setDetailEvent({ summary: parsed.summary, raw: parsed.rawData! }) : undefined}
            >
              <span className="text-muted-foreground text-xs shrink-0 mt-0.5 w-[64px] tabular-nums font-mono">
                {time}
              </span>
              <Badge
                variant={config.variant}
                className={cn('shrink-0 w-[52px] justify-center text-[10px] font-semibold px-1 py-0 gap-0.5', config.className)}
              >
                <Icon className="h-2.5 w-2.5" />
                {config.label}
              </Badge>
              <span className={cn(
                'leading-snug flex-1',
                hasDetail ? 'text-foreground/90 underline decoration-dotted decoration-muted-foreground/30 underline-offset-2' : 'text-foreground/90'
              )}>
                {parsed.summary}
              </span>
              {hasDetail && (
                <ExternalLink className="h-3 w-3 text-primary/60 shrink-0 mt-0.5" />
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
