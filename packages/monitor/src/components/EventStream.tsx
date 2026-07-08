import { useState, useEffect, useRef } from 'react'
import { groupBy, mapValues, omit } from 'es-toolkit'
import { Button, Separator } from '@metalbear/ui'
import { Activity, Download, Trash2 } from 'lucide-react'
import type { SessionInfo, MonitorEvent } from '../types'
import type { EventTypeValue } from '../eventTypes'
import { strings } from '../strings'
import { api } from '../api'
import { trackEvent } from '../analytics'
import EventFilterChips from './events/EventFilterChips'
import EventSearchBar from './events/EventSearchBar'
import EventRow from './events/EventRow'
import EventDetailDialog from './events/EventDetailDialog'
import { MAX_EVENTS } from './events/eventConfig'
import { parseEvent, type ParsedEvent } from './events/parseEvent'
import { buildExportZip } from '../export'

interface TimestampedEvent {
  event: MonitorEvent
  receivedAt: Date
}

interface Props {
  session: SessionInfo
}

// Display-only shaping of an event for the detail dialog. The exported session log keeps the
// original event: the exchange correlation id is internal plumbing for pairing events, and
// headers stay an array of pairs there because names can repeat (e.g. set-cookie) and HAR
// requires the pair shape. For reading, the id is noise and headers are friendlier as a
// key/value object, with repeated names collapsing into an array of values.
const toDisplayEvent = (raw: unknown): unknown => {
  if (!raw || typeof raw !== 'object') return raw
  const { headers, ...rest } = omit(raw as Record<string, unknown>, ['id'])
  if (!Array.isArray(headers)) return rest
  const pairs = headers.filter(
    (pair): pair is [string, string] => Array.isArray(pair) && pair.length === 2,
  )
  const grouped = mapValues(
    groupBy(pairs, ([name]) => name),
    (group) => {
      const values = group.map(([, value]) => value)
      return values.length === 1 ? values[0] : values
    },
  )
  return { ...rest, headers: grouped }
}

export default function EventStream({ session }: Props) {
  const [events, setEvents] = useState<TimestampedEvent[]>([])
  const [streaming, setStreaming] = useState(false)
  const [detailEvent, setDetailEvent] = useState<MonitorEvent | null>(null)
  const [searchQuery, setSearchQuery] = useState('')
  const [activeFilter, setActiveFilter] = useState<EventTypeValue | null>(null)
  const logRef = useRef<HTMLDivElement>(null)
  // Total events received for this session, including ones already evicted by the
  // MAX_EVENTS cap; lets the export record how many events it is missing.
  const seenRef = useRef(0)

  useEffect(() => {
    setEvents([])
    seenRef.current = 0
    setStreaming(true)

    const eventSource = new EventSource(api.eventStreamUrl(session.session_id))

    eventSource.onmessage = (e) => {
      let event: MonitorEvent
      try {
        event = JSON.parse(e.data)
      } catch {
        return
      }
      seenRef.current += 1
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

  // One click exports a single zip with the session log (every event, including DNS/file/layer)
  // and a HAR of the HTTP exchanges (replayable in DevTools). Built and clicked synchronously
  // inside the gesture so the browser treats the download as user-initiated.
  const exportLog = () => {
    trackEvent('session_monitor_export_log', {
      session_id: session.session_id,
      event_count: events.length,
    })
    const { filename, data } = buildExportZip(session, events, {
      droppedEvents: Math.max(0, seenRef.current - events.length),
      exportedAt: new Date(),
    })
    const blob = new Blob([data as BlobPart], { type: 'application/zip' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = filename
    a.style.display = 'none'
    document.body.appendChild(a)
    a.click()
    document.body.removeChild(a)
    setTimeout(() => URL.revokeObjectURL(url), 10_000)
  }

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

  // Events that have raw detail to show; the detail dialog navigates within this list, keyed
  // by event identity so live appends and MAX_EVENTS eviction don't shift what is displayed.
  const detailableEvents = filteredEvents.filter(({ parsed }) => parsed.rawData !== undefined)
  const detailIndex =
    detailEvent === null
      ? -1
      : detailableEvents.findIndex(({ event }) => event === detailEvent)
  const detailEntry = detailIndex >= 0 ? detailableEvents[detailIndex] : null

  const navigateDetail = (delta: number) => {
    const next = detailableEvents[detailIndex + delta]
    if (next) setDetailEvent(next.event)
  }

  const countLabel = activeFilter !== null || searchQuery
    ? `${filteredEvents.length}/${processedEvents.length}`
    : `${filteredEvents.length}`

  const hasEvents = processedEvents.length > 0

  return (
    <div className="h-full flex flex-col">
      <div className="border-b border-border px-4 py-2 surface-inset flex items-center gap-3">
        {hasEvents && (
          <div className="flex items-center gap-2 flex-1">
            <EventSearchBar query={searchQuery} onChange={setSearchQuery} />
          </div>
        )}

        {hasEvents && (
          <>
            <EventFilterChips
              activeFilter={activeFilter}
              onChange={setActiveFilter}
            />
            <Separator orientation="vertical" className="h-3" />
          </>
        )}

        <span className="text-meta text-muted-foreground tabular-nums ml-auto inline-flex items-center gap-1.5">
          {!hasEvents && streaming && (
            <Activity className="h-3 w-3 opacity-50 animate-pulse" />
          )}
          {hasEvents
            ? `${countLabel} ${strings.events.countSuffix}`
            : streaming
              ? strings.events.waiting
              : `0 ${strings.events.countSuffix}`}
        </span>

        <Button
          variant="ghost"
          size="icon"
          onClick={exportLog}
          title={strings.events.export}
          aria-label={strings.events.export}
          className="h-6 w-6"
        >
          <Download className="h-3.5 w-3.5" />
        </Button>

        {hasEvents && (
          <Button
            variant="ghost"
            size="icon"
            onClick={() => {
              setEvents([])
              seenRef.current = 0
            }}
            title={strings.events.clear}
            className="h-6 w-6"
          >
            <Trash2 className="h-3.5 w-3.5" />
          </Button>
        )}
      </div>

      <div ref={logRef} className="flex-1 overflow-y-auto text-xs font-mono">
        {filteredEvents.length === 0 && hasEvents && (
          <div className="text-muted-foreground text-center py-4 text-meta">
            No events match the current filter.
          </div>
        )}
        {filteredEvents.map(({ event, parsed, receivedAt }, i) => (
          <EventRow
            key={`${receivedAt.getTime()}-${i}`}
            parsed={parsed}
            receivedAt={receivedAt}
            zebra={i % 2 === 0}
            onClick={parsed.rawData !== undefined ? () => setDetailEvent(event) : undefined}
          />
        ))}
      </div>

      <EventDetailDialog
        detail={
          detailEntry
            ? {
                summary: detailEntry.parsed.summary,
                raw: toDisplayEvent(detailEntry.parsed.rawData),
                position: { current: detailIndex + 1, total: detailableEvents.length },
              }
            : null
        }
        onNavigate={navigateDetail}
        onOpenChange={() => setDetailEvent(null)}
      />
    </div>
  )
}
