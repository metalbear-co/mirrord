import { useState, useEffect, useRef } from 'react'
import { Button, Separator } from '@metalbear/ui'
import { Activity, Trash2 } from 'lucide-react'
import type { SessionInfo, MonitorEvent } from '../types'
import type { EventTypeValue } from '../eventTypes'
import { strings } from '../strings'
import { api } from '../api'
import EventFilterChips from './events/EventFilterChips'
import EventSearchBar from './events/EventSearchBar'
import EventRow from './events/EventRow'
import EventDetailDialog from './events/EventDetailDialog'
import { MAX_EVENTS } from './events/eventConfig'
import { parseEvent, type ParsedEvent } from './events/parseEvent'

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

  const countLabel = activeFilter !== null || searchQuery
    ? `${filteredEvents.length}/${processedEvents.length}`
    : `${filteredEvents.length}`

  return (
    <div className="h-full flex flex-col">
      <div className="border-b border-border px-4 py-2 bg-card/30 flex items-center gap-3">
        <div className="flex items-center gap-2 flex-1">
          <EventSearchBar query={searchQuery} onChange={setSearchQuery} />
        </div>

        <EventFilterChips activeFilter={activeFilter} onChange={setActiveFilter} />

        <Separator orientation="vertical" className="h-3" />

        <span className="text-[10px] text-muted-foreground tabular-nums">
          {countLabel} {strings.events.countSuffix}
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
        {filteredEvents.map(({ parsed, receivedAt }, i) => (
          <EventRow
            key={`${receivedAt.getTime()}-${i}`}
            parsed={parsed}
            receivedAt={receivedAt}
            zebra={i % 2 === 0}
            onClick={
              parsed.rawData
                ? () => setDetailEvent({ summary: parsed.summary, raw: parsed.rawData! })
                : undefined
            }
          />
        ))}
      </div>

      <EventDetailDialog detail={detailEvent} onOpenChange={() => setDetailEvent(null)} />
    </div>
  )
}
