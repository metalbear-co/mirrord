import React, { useState, useEffect, useRef } from 'react'
import { Badge, Separator, cn } from '@metalbear/ui'
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@metalbear/ui'
import { FileText, Globe, Zap, Activity, Server, Info, AlertTriangle, Bug, Settings, ExternalLink, Trash2, Search, X } from 'lucide-react'
import type { SessionInfo, SessionEvent } from './types'

const EVENT_TYPE_CONFIG: Record<string, {
  variant: 'default' | 'destructive' | 'outline' | 'secondary'
  label: string
  className: string
  icon: typeof Activity
}> = {
  file_op: { variant: 'outline', label: 'File', className: 'border-amber-500/50 text-amber-400 bg-amber-500/10', icon: FileText },
  dns_query: { variant: 'outline', label: 'DNS', className: 'border-blue-500/50 text-blue-400 bg-blue-500/10', icon: Globe },
  http_request: { variant: 'outline', label: 'HTTP', className: 'border-green-500/50 text-green-400 bg-green-500/10', icon: Zap },
  connection: { variant: 'outline', label: 'Event', className: 'border-purple-500/50 text-purple-400 bg-purple-500/10', icon: Server },
  env_var: { variant: 'outline', label: 'Env', className: 'border-pink-500/50 text-pink-400 bg-pink-500/10', icon: Settings },
  info: { variant: 'outline', label: 'Info', className: 'border-sky-500/50 text-sky-400 bg-sky-500/10', icon: Info },
  warning: { variant: 'outline', label: 'Warn', className: 'border-yellow-500/50 text-yellow-400 bg-yellow-500/10', icon: AlertTriangle },
  error: { variant: 'outline', label: 'Error', className: 'border-red-500/50 text-red-400 bg-red-500/10', icon: Bug },
}

interface ParsedEvent {
  type: string
  summary: string
  rawData?: string
  skip: boolean
}

function parseEvent(event: SessionEvent): ParsedEvent {
  switch (event.type) {
    case 'file_op': {
      const op = (event.operation as string) || 'op'
      const path = (event.path as string | null) ?? null
      const summary = path ? `${op} ${path}` : `${op} (fd-based)`
      return { type: 'file_op', summary, skip: false }
    }
    case 'dns_query': {
      const host = (event.host as string) || '?'
      return { type: 'dns_query', summary: `DNS lookup: ${host}`, skip: false }
    }
    case 'outgoing_connection': {
      const addr = (event.address as string) || '?'
      const port = (event.port as number) ?? ''
      return { type: 'connection', summary: `Connect to ${addr}:${port}`, skip: false }
    }
    case 'port_subscription': {
      const port = (event.port as number) ?? '?'
      const mode = (event.mode as string) || ''
      return { type: 'connection', summary: `Port ${port} subscribed (${mode})`, skip: false }
    }
    case 'env_var': {
      const vars = (event.vars as string[]) ?? []
      const summary =
        vars.length === 0
          ? 'Env vars requested'
          : `Env vars: ${vars.slice(0, 5).join(', ')}${vars.length > 5 ? ` +${vars.length - 5} more` : ''}`
      return { type: 'env_var', summary, skip: false }
    }
    case 'layer_connected': {
      const pid = (event.pid as number) ?? '?'
      return { type: 'info', summary: `Process connected (PID ${pid})`, skip: false }
    }
    case 'layer_disconnected': {
      return { type: 'info', summary: 'Process disconnected', skip: false }
    }
    default:
      return { type: 'info', summary: `${event.type}`, skip: false }
  }
}

/** Format time as 24-hour HH:MM:SS */
function formatTime24(timestamp: string): string {
  const d = new Date(timestamp)
  return d.toLocaleTimeString('en-GB', { hour12: false })
}

/** Try to pretty-format raw data (JSON or Rust struct) */
function formatRawData(raw: string): { formatted: string; isStructured: boolean } {
  // Try JSON parse first
  try {
    const parsed = JSON.parse(raw)
    return { formatted: JSON.stringify(parsed, null, 2), isStructured: true }
  } catch {}

  // Try to detect if it looks like a JSON-ish structure that just needs cleanup
  const jsonLike = raw.trim()
  if (jsonLike.startsWith('{') || jsonLike.startsWith('[')) {
    try {
      // Sometimes raw data has trailing commas or other minor issues
      const cleaned = jsonLike.replace(/,\s*([}\]])/g, '$1')
      const parsed = JSON.parse(cleaned)
      return { formatted: JSON.stringify(parsed, null, 2), isStructured: true }
    } catch {}
  }

  // Rust struct formatting: indent based on braces/parens
  if (raw.includes('{') && (raw.includes('::') || raw.includes('Some(') || raw.includes('None'))) {
    let indent = 0
    let result = ''
    let i = 0
    while (i < raw.length) {
      const ch = raw[i]
      if (ch === '{' || ch === '(') {
        result += ch + '\n'
        indent += 2
        result += ' '.repeat(indent)
        // Skip whitespace after opening brace
        i++
        while (i < raw.length && (raw[i] === ' ' || raw[i] === '\n')) i++
        continue
      } else if (ch === '}' || ch === ')') {
        indent = Math.max(0, indent - 2)
        result += '\n' + ' '.repeat(indent) + ch
      } else if (ch === ',') {
        result += ',\n' + ' '.repeat(indent)
        // Skip whitespace after comma
        i++
        while (i < raw.length && (raw[i] === ' ' || raw[i] === '\n')) i++
        continue
      } else {
        result += ch
      }
      i++
    }
    return { formatted: result, isStructured: true }
  }

  // If it's already multi-line or has some structure, mark it as structured
  if (raw.includes('\n') && raw.split('\n').length > 3) {
    return { formatted: raw, isStructured: true }
  }

  return { formatted: raw, isStructured: false }
}

/** Simple syntax colorizer for config lines */
function colorizeConfigLine(line: string): React.ReactNode {
  // Color key: value patterns
  const kvMatch = line.match(/^(\s*)([\w_]+):\s*(.+)$/)
  if (kvMatch) {
    const [, indent, key, value] = kvMatch
    let valueColor = '#a6e3a1' // green for strings
    if (value === 'true' || value === 'false') valueColor = '#fab387' // orange for booleans
    else if (value === 'None' || value === 'null') valueColor = '#585b70' // dim for null
    else if (/^\d+/.test(value)) valueColor = '#f9e2af' // yellow for numbers
    else if (value.startsWith('"')) valueColor = '#a6e3a1' // green for strings
    return (
      <>
        {indent}
        <span style={{ color: '#89b4fa' }}>{key}</span>
        <span style={{ color: '#585b70' }}>: </span>
        <span style={{ color: valueColor }}>{value}</span>
      </>
    )
  }
  // Braces/brackets
  if (/^\s*[{}()\[\]],?\s*$/.test(line)) {
    return <span style={{ color: '#585b70' }}>{line}</span>
  }
  // Struct names like "LayerConfig {" or "FeatureConfig {"
  const structMatch = line.match(/^(\s*)(\w+)\s*(\{.*)$/)
  if (structMatch) {
    const [, indent, name, rest] = structMatch
    return (
      <>
        {indent}
        <span style={{ color: '#cba6f7' }}>{name}</span>
        <span style={{ color: '#585b70' }}>{rest}</span>
      </>
    )
  }
  return line
}

interface Props {
  session: SessionInfo
}

export default function EventStream({ session }: Props) {
  const [events, setEvents] = useState<SessionEvent[]>([])
  const [streaming, setStreaming] = useState(false)
  const [detailEvent, setDetailEvent] = useState<{ summary: string; raw: string } | null>(null)
  const [searchQuery, setSearchQuery] = useState('')
  const [showSearch, setShowSearch] = useState(false)
  const logRef = useRef<HTMLDivElement>(null)
  const searchRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    setEvents([])
    setStreaming(true)

    const baseUrl = import.meta.env.DEV ? 'http://localhost:59281' : ''
    const eventSource = new EventSource(`${baseUrl}/api/sessions/${session.session_id}/events`)

    eventSource.onmessage = (e) => {
      const raw = JSON.parse(e.data)
      const event: SessionEvent = { ...raw, received_at: Date.now() }
      setEvents((prev) => [...prev.slice(-200), event])
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

  // Auto-scroll only if user is already near the bottom
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

  // Parse, filter, and dedup
  const processedEvents = events
    .map((event) => ({ event, parsed: parseEvent(event) }))
    .filter(({ parsed }) => !parsed.skip)

  // Dedup: only dedup info/connection startup messages, not traffic events
  const TRAFFIC_TYPES = new Set(['file_op', 'dns_query', 'http_request', 'env_var'])
  const seenSummaries = new Set<string>()
  const dedupedEvents = processedEvents.filter(({ parsed }) => {
    // Never dedup traffic events (file, dns, http, env) - they should all show
    if (TRAFFIC_TYPES.has(parsed.type)) return true
    // Dedup info/connection/warning startup messages
    if (seenSummaries.has(parsed.summary)) return false
    seenSummaries.add(parsed.summary)
    return true
  })

  // Search filter
  const filteredEvents = searchQuery
    ? dedupedEvents.filter(({ parsed }) =>
        parsed.summary.toLowerCase().includes(searchQuery.toLowerCase())
      )
    : dedupedEvents

  // Format detail event raw data for dialog
  const detailFormatted = detailEvent ? formatRawData(detailEvent.raw) : null

  return (
    <div className="h-full flex flex-col">
      {/* Session detail header */}
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

      {/* Search bar */}
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
            {filteredEvents.length}/{dedupedEvents.length}
          </span>
        </div>
      )}

      {/* Event log */}
      <div ref={logRef} className="flex-1 overflow-y-auto p-4 text-sm">
        {filteredEvents.length === 0 && streaming && (
          <div className="text-muted-foreground text-center py-8 flex flex-col items-center gap-2">
            <Activity className="h-6 w-6 opacity-30 animate-pulse" />
            <span>Waiting for events...</span>
          </div>
        )}
        {filteredEvents.map(({ event, parsed }, i) => {
          const config = EVENT_TYPE_CONFIG[parsed.type] ?? EVENT_TYPE_CONFIG['connection']!
          const time = formatTime24(new Date(event.received_at).toISOString())
          const Icon = config.icon
          const hasDetail = !!parsed.rawData

          return (
            <div
              key={`${event.timestamp}-${i}`}
              className={cn(
                'flex items-start gap-2.5 py-1 px-3 rounded-md transition-colors event-row-animate',
                hasDetail ? 'hover:bg-muted/50 cursor-pointer group' : 'hover:bg-muted/30',
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

      {/* Detail dialog */}
      <Dialog open={!!detailEvent} onOpenChange={() => setDetailEvent(null)}>
        <DialogContent className="max-w-4xl max-h-[85vh]">
          <DialogHeader>
            <DialogTitle className="text-sm font-medium text-foreground">
              {detailEvent?.summary}
            </DialogTitle>
          </DialogHeader>
          <div className="relative overflow-auto max-h-[70vh] rounded-lg border border-border bg-[#1e1e2e] text-[#cdd6f4]">
            <div className="overflow-x-auto">
              <table className="w-full border-collapse text-xs font-mono">
                <tbody>
                  {detailFormatted?.formatted.split('\n').map((line, lineIdx) => (
                    <tr key={lineIdx} className="hover:bg-white/5">
                      <td className="select-none text-right pr-4 pl-4 py-[2px] text-[#585b70] border-r border-[#313244] w-[1%] whitespace-nowrap">
                        {lineIdx + 1}
                      </td>
                      <td className="pl-4 pr-4 py-[2px] whitespace-pre-wrap break-all">
                        {colorizeConfigLine(line)}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  )
}
