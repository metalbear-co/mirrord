import { useState, useEffect, useRef } from 'react'
import { Button, Separator } from '@metalbear/ui'
import { Activity, Trash2 } from 'lucide-react'
import type { SessionInfo, MonitorEvent, ClientChaosRule } from '../types'
import { EventType, type EventTypeValue } from '../eventTypes'
import { strings } from '../strings'
import { api } from '../api'
import type { ChaosRuleFields } from '../hooks/useChaosRules'
import EventFilterChips from './events/EventFilterChips'
import EventSearchBar from './events/EventSearchBar'
import EventRow, { type EventChaosTag } from './events/EventRow'
import EventDetailDialog from './events/EventDetailDialog'
import { MAX_EVENTS } from './events/eventConfig'
import { parseEvent, type ParsedEvent } from './events/parseEvent'
import { formatHostPort } from '../utils'
import BreakPopover from './chaos/BreakPopover'
import { matchChaosRule, ruleDisplayName } from './chaos/chaosMatch'

const NEAR_BOTTOM_THRESHOLD_PX = 50

interface TimestampedEvent {
  event: MonitorEvent
  receivedAt: Date
  seq: number
}

interface Props {
  session: SessionInfo
  chaosRules?: ClientChaosRule[] | undefined
  onBreakRule?: ((fields: ChaosRuleFields) => Promise<void>) | undefined
  onBreakMore?: ((host: string) => void) | undefined
}

export default function EventStream({
  session,
  chaosRules,
  onBreakRule,
  onBreakMore,
}: Props) {
  const [events, setEvents] = useState<TimestampedEvent[]>([])
  const [streaming, setStreaming] = useState(false)
  const [detailEvent, setDetailEvent] = useState<{
    summary: string
    raw: string
  } | null>(null)
  const [searchQuery, setSearchQuery] = useState('')
  const [activeFilter, setActiveFilter] = useState<EventTypeValue | null>(null)
  const [affectedOnly, setAffectedOnly] = useState(false)
  const [breakHost, setBreakHost] = useState<string | null>(null)
  const logRef = useRef<HTMLDivElement>(null)
  const seqRef = useRef(0)

  useEffect(() => {
    setEvents([])
    setStreaming(true)

    const eventSource = new EventSource(api.eventStreamUrl(session.session_id))

    eventSource.onmessage = (e) => {
      let event: MonitorEvent
      try {
        event = JSON.parse(e.data as string) as MonitorEvent
      } catch {
        return
      }
      setEvents((prev) => {
        // Append the new event and cap the buffer at MAX_EVENTS by dropping
        // the oldest entries. Keeps memory bounded for long-running sessions.
        const next = [
          ...prev,
          { event, receivedAt: new Date(), seq: seqRef.current++ },
        ]
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
  // Auto-scroll pauses while the pointer is inside the log. Without this, live
  // traffic shifts the rows several times a second and the hover-revealed
  // "Break this" button escapes the cursor before it can be clicked.
  const pointerInside = useRef(false)
  useEffect(() => {
    const logEl = logRef.current
    if (!logEl) return
    const handleScroll = () => {
      isNearBottom.current =
        logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight <
        NEAR_BOTTOM_THRESHOLD_PX
    }
    const handleEnter = () => {
      pointerInside.current = true
    }
    const handleLeave = () => {
      pointerInside.current = false
    }
    logEl.addEventListener('scroll', handleScroll)
    logEl.addEventListener('pointerenter', handleEnter)
    logEl.addEventListener('pointerleave', handleLeave)
    return () => {
      logEl.removeEventListener('scroll', handleScroll)
      logEl.removeEventListener('pointerenter', handleEnter)
      logEl.removeEventListener('pointerleave', handleLeave)
    }
  }, [])

  useEffect(() => {
    if (
      logRef.current &&
      isNearBottom.current &&
      !pointerInside.current &&
      !breakHost
    ) {
      logRef.current.scrollTop = logRef.current.scrollHeight
    }
  }, [events, breakHost])

  // parseEvent returns null for events that aren't displayed in the stream
  // (port_subscription/env_var are shown in Overview, plus malformed events).
  const processedEvents = events
    .map(({ event, receivedAt, seq }) => ({
      event,
      receivedAt,
      seq,
      parsed: parseEvent(event),
    }))
    .filter(
      (
        e,
      ): e is {
        event: MonitorEvent
        receivedAt: Date
        seq: number
        parsed: ParsedEvent
      } => e.parsed !== null,
    )
    .map((e) => {
      const matched =
        chaosRules && e.event.type === EventType.OutgoingConnection
          ? matchChaosRule(chaosRules, e.event.address, e.event.port)
          : null
      return { ...e, matched }
    })

  const filteredEvents = processedEvents.filter(({ parsed, matched }) => {
    const matchesType =
      (activeFilter === null || parsed.type === activeFilter) &&
      (!affectedOnly || matched !== null)
    const matchesSearch =
      !searchQuery ||
      parsed.summary.toLowerCase().includes(searchQuery.toLowerCase())
    return matchesType && matchesSearch
  })

  const countLabel =
    activeFilter !== null || searchQuery
      ? `${filteredEvents.length}/${processedEvents.length}`
      : `${filteredEvents.length}`

  const hasEvents = processedEvents.length > 0

  return (
    <div className="relative flex h-full flex-col">
      <div className="border-border surface-inset flex items-center gap-3 border-b px-4 py-2">
        {hasEvents && (
          <div className="flex flex-1 items-center gap-2">
            <EventSearchBar query={searchQuery} onChange={setSearchQuery} />
          </div>
        )}

        {hasEvents && (
          <>
            <EventFilterChips
              activeFilter={activeFilter}
              onChange={(filter) => {
                setActiveFilter(filter)
                setAffectedOnly(false)
              }}
              affectedActive={affectedOnly}
              onAffectedChange={
                chaosRules
                  ? (active) => {
                      setAffectedOnly(active)
                      if (active) setActiveFilter(null)
                    }
                  : undefined
              }
            />
            <Separator orientation="vertical" className="h-3" />
          </>
        )}

        <span className="text-meta text-muted-foreground ml-auto inline-flex items-center gap-1.5 tabular-nums">
          {!hasEvents && streaming && (
            <Activity className="h-3 w-3 animate-pulse opacity-50" />
          )}
          {hasEvents
            ? `${countLabel} ${strings.events.countSuffix}`
            : streaming
              ? strings.events.waiting
              : `0 ${strings.events.countSuffix}`}
        </span>

        {hasEvents && (
          <Button
            variant="ghost"
            size="icon"
            onClick={() => setEvents([])}
            title={strings.events.clear}
            className="h-6 w-6"
          >
            <Trash2 className="h-3.5 w-3.5" />
          </Button>
        )}
      </div>

      <div ref={logRef} className="flex-1 overflow-y-auto font-mono text-xs">
        {filteredEvents.length === 0 && hasEvents && (
          <div className="text-muted-foreground text-meta py-4 text-center">
            {strings.events.noFilterMatch}
          </div>
        )}
        {filteredEvents.map(
          ({ event, parsed, receivedAt, seq, matched }, i) => {
            const rawData = parsed.rawData
            const chaosTag: EventChaosTag | null = matched
              ? {
                  name: ruleDisplayName(matched),
                  isError: matched.effectKind !== 'latency',
                }
              : null
            const canBreak =
              !matched &&
              onBreakRule &&
              event.type === EventType.OutgoingConnection
            return (
              <EventRow
                key={seq}
                parsed={parsed}
                receivedAt={receivedAt}
                zebra={i % 2 === 0}
                chaosTag={chaosTag}
                onBreak={
                  canBreak
                    ? () =>
                        setBreakHost(formatHostPort(event.address, event.port))
                    : null
                }
                onClick={
                  rawData
                    ? () =>
                        setDetailEvent({
                          summary: parsed.summary,
                          raw: rawData,
                        })
                    : undefined
                }
              />
            )
          },
        )}
      </div>

      {breakHost && onBreakRule && (
        <BreakPopover
          host={breakHost}
          onClose={() => setBreakHost(null)}
          onArm={async (fields) => {
            await onBreakRule(fields)
            setBreakHost(null)
          }}
          onMore={(host) => {
            setBreakHost(null)
            onBreakMore?.(host)
          }}
        />
      )}

      <EventDetailDialog
        detail={detailEvent}
        onOpenChange={() => setDetailEvent(null)}
      />
    </div>
  )
}
