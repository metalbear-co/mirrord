import { useState, useEffect, useRef } from 'react'
import { groupBy, mapValues, omit } from 'es-toolkit'
import { Badge, Button, Switch, cn } from '@metalbear/ui'
import { Activity, Download, Pause, Play, Trash2 } from 'lucide-react'
import type { SessionInfo, MonitorEvent } from '../types'
import { strings } from '../strings'
import { api } from '../api'
import { trackEvent } from '../analytics'
import EventFilterChips, { type EventFilter } from './events/EventFilterChips'
import EventSearchBar from './events/EventSearchBar'
import EventRow, { ROW_GRID } from './events/EventRow'
import InspectorPane from './events/InspectorPane'
import ResizableSplit from './ResizableSplit'
import { MAX_EVENTS } from './events/eventConfig'
import { EventType } from '../eventTypes'
import { formatBytes, parseEvent, type ParsedEvent } from './events/parseEvent'
import { buildExportZip } from '../export'

interface TimestampedEvent {
  event: MonitorEvent
  receivedAt: Date
}

interface ProcessedEvent {
  event: MonitorEvent
  receivedAt: Date
  parsed: ParsedEvent
  // Time from the request event to its response event for the same exchange, measured at the
  // proxy. Approximate but enough to spot the slow endpoint.
  durationMs?: number
}

// One rendered table row: the latest event of a consecutive run of same-shaped events,
// with the run length. When "group repeats" is off every run has length 1.
interface DisplayRow {
  entry: ProcessedEvent
  count: number
}

interface Props {
  session: SessionInfo
}

// Display-only shaping of an event for the inspector. The exported session log keeps the
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

const isEditableTarget = (target: EventTarget | null): boolean =>
  target instanceof HTMLElement &&
  target.closest('input, textarea, select, [contenteditable="true"]') !== null

const asRecord = (value: unknown): Record<string, unknown> | null =>
  value && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : null

// An HTTP-filtered steal session shows nothing until traffic matches the filter, which reads
// as broken to anyone who doesn't know the filter is there. Spell the filter out (with a
// ready-made request when it's a header filter) instead of a bare spinner.
function WaitingHint({ session }: { session: SessionInfo }) {
  const incoming = asRecord(asRecord(asRecord(asRecord(session.config)?.feature)?.network)?.incoming)
  const httpFilter = asRecord(incoming?.http_filter)
  const headerFilter = typeof httpFilter?.header_filter === 'string' ? httpFilter.header_filter : null
  const pathFilter = typeof httpFilter?.path_filter === 'string' ? httpFilter.path_filter : null
  const filter = headerFilter ?? pathFilter
  if (!filter) return null

  return (
    <div className="flex flex-col items-center gap-2 py-8 text-meta text-muted-foreground">
      <span>
        {strings.events.waitingFilterHint}{' '}
        <Badge variant="outline" className="font-mono">
          {filter}
        </Badge>
      </span>
      {headerFilter && (
        <span className="font-mono text-[11px] bg-muted/40 border border-border rounded-md px-2.5 py-1 select-all">
          curl -H '{headerFilter}' https://&lt;your-app&gt;/path
        </span>
      )}
    </div>
  )
}

export default function EventStream({ session }: Props) {
  const [events, setEvents] = useState<TimestampedEvent[]>([])
  const [streaming, setStreaming] = useState(false)
  const [detailEvent, setDetailEvent] = useState<{ event: MonitorEvent; groupKey: string } | null>(
    null,
  )
  const [searchQuery, setSearchQuery] = useState('')
  const [activeFilter, setActiveFilter] = useState<EventFilter>(null)
  const [groupRepeats, setGroupRepeats] = useState(true)
  // While paused the list renders this frozen snapshot; events keep buffering behind it so
  // resuming shows everything that arrived in the meantime.
  const [pausedEvents, setPausedEvents] = useState<TimestampedEvent[] | null>(null)
  const logRef = useRef<HTMLDivElement>(null)
  // Total events received for this session, including ones already evicted by the
  // MAX_EVENTS cap; lets the export record how many events it is missing.
  const seenRef = useRef(0)

  useEffect(() => {
    setEvents([])
    setPausedEvents(null)
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

  const visibleEvents = pausedEvents ?? events

  // parseEvent returns null for events that aren't displayed in the stream
  // (port_subscription/env_var are shown in Overview, plus malformed events).
  const processedEvents = visibleEvents
    .map(({ event, receivedAt }) => ({
      event,
      receivedAt,
      parsed: parseEvent(event),
    }))
    .filter((e): e is ProcessedEvent => e.parsed !== null)

  // Pair each response with its request via the shared exchange id to measure how long the
  // exchange took at the proxy.
  const requestTimes = new Map<string, number>()
  for (const entry of processedEvents) {
    if (entry.event.type === EventType.IncomingRequest) {
      requestTimes.set(entry.event.id, entry.receivedAt.getTime())
    } else if (entry.event.type === EventType.IncomingResponse) {
      const started = requestTimes.get(entry.event.id)
      if (started !== undefined) {
        entry.durationMs = Math.max(0, entry.receivedAt.getTime() - started)
      }
    }
  }

  // Fold a body event into its head request/response row via the shared exchange id, so one
  // request or response renders as a single row and the inspector shows headers and body
  // together. Bodies stream in after their head with other exchanges possibly interleaved,
  // hence the small lookback; a body whose head fell outside it still renders standalone.
  const MERGE_LOOKBACK = 12
  const mergedEvents: ProcessedEvent[] = []
  for (const entry of processedEvents) {
    const { event } = entry
    const bodyEvent =
      event.type === EventType.IncomingRequestBody || event.type === EventType.IncomingResponseBody
        ? event
        : null
    if (bodyEvent) {
      const headKind =
        bodyEvent.type === EventType.IncomingRequestBody
          ? EventType.IncomingRequest
          : EventType.IncomingResponse
      const start = Math.max(0, mergedEvents.length - MERGE_LOOKBACK)
      let merged = false
      for (let i = mergedEvents.length - 1; i >= start; i--) {
        const head = mergedEvents[i]
        if (head.event.type === headKind && 'id' in head.event && head.event.id === bodyEvent.id) {
          const size = `${formatBytes(bodyEvent.bytes)}${bodyEvent.truncated ? ' · truncated' : ''}`
          const bodyRaw = entry.parsed.rawData as Record<string, unknown> | null
          const headRaw = (head.parsed.rawData ?? head.event) as Record<string, unknown>
          mergedEvents[i] = {
            ...head,
            parsed: {
              ...head.parsed,
              summary: `${head.parsed.summary} · body (${size})`,
              columns: {
                ...head.parsed.columns,
                path: `${head.parsed.columns.path} · ${size}`,
              },
              rawData: {
                ...headRaw,
                body: bodyRaw?.body,
                bytes: bodyEvent.bytes,
                truncated: bodyEvent.truncated,
              },
            },
          }
          merged = true
          break
        }
      }
      if (merged) continue
    }
    mergedEvents.push(entry)
  }

  const typeCounts: Partial<Record<string, number>> = {}
  let errorCount = 0
  for (const { parsed } of mergedEvents) {
    typeCounts[parsed.type] = (typeCounts[parsed.type] ?? 0) + 1
    if (parsed.columns.statusTone === 'error') errorCount += 1
  }

  const filteredEvents = mergedEvents.filter(({ parsed }) => {
    const matchesType =
      activeFilter === null ||
      (activeFilter === 'errors'
        ? parsed.columns.statusTone === 'error'
        : parsed.type === activeFilter)
    const matchesSearch = !searchQuery || parsed.summary.toLowerCase().includes(searchQuery.toLowerCase())
    return matchesType && matchesSearch
  })

  // Collapse repeated events into one row showing the repeat's latest occurrence, so health
  // probes and polling loops don't drown the feed. Strict adjacency would never group real
  // traffic: one HTTP exchange fans out into request / response / body events whose kinds
  // interleave, so a repeating probe produces a repeating CYCLE of distinct keys. A small
  // lookback window over the most recent rows collapses such cycles while staying local
  // enough that genuinely separate traffic further up the feed is never folded together.
  const LOOKBACK_ROWS = 8
  const rows: DisplayRow[] = []
  for (const entry of filteredEvents) {
    let merged = false
    if (groupRepeats) {
      const start = Math.max(0, rows.length - LOOKBACK_ROWS)
      for (let i = rows.length - 1; i >= start; i--) {
        if (rows[i].entry.parsed.groupKey === entry.parsed.groupKey) {
          rows[i] = { entry, count: rows[i].count + 1 }
          merged = true
          break
        }
      }
    }
    if (!merged) rows.push({ entry, count: 1 })
  }

  const detailableRows = rows.filter(({ entry }) => entry.parsed.rawData !== undefined)
  // Selection is keyed by event identity, falling back to the run's groupKey so the inspector
  // follows a grouped row as new repeats replace its latest event.
  const detailIndex =
    detailEvent === null
      ? -1
      : (() => {
          const byIdentity = detailableRows.findIndex(({ entry }) => entry.event === detailEvent.event)
          if (byIdentity >= 0 || !groupRepeats) return byIdentity
          return detailableRows.findIndex(
            ({ entry }) => entry.parsed.groupKey === detailEvent.groupKey,
          )
        })()
  const detailRow = detailIndex >= 0 ? detailableRows[detailIndex] : null

  const selectRow = (row: DisplayRow) =>
    setDetailEvent({ event: row.entry.event, groupKey: row.entry.parsed.groupKey })

  const navigateDetail = (delta: number) => {
    const next = detailableRows[detailIndex + delta]
    if (next) selectRow(next)
  }

  useEffect(() => {
    if (!detailEvent) return
    const onKeyDown = (e: KeyboardEvent) => {
      if (isEditableTarget(e.target)) return
      if (e.key === 'ArrowDown' || e.key === 'ArrowRight') {
        e.preventDefault()
        navigateDetail(1)
      } else if (e.key === 'ArrowUp' || e.key === 'ArrowLeft') {
        e.preventDefault()
        navigateDetail(-1)
      } else if (e.key === 'Escape') {
        setDetailEvent(null)
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  })

  const countLabel = activeFilter !== null || searchQuery
    ? `${filteredEvents.length}/${mergedEvents.length}`
    : `${filteredEvents.length}`

  const hasEvents = mergedEvents.length > 0

  const table = (
    <div className="h-full min-w-0 flex flex-col bg-card border border-border rounded-lg overflow-hidden">
        <div className="border-b border-border px-4 py-2 flex items-center gap-3">
          <span className="text-body font-semibold whitespace-nowrap">Events</span>

          {hasEvents && (
            <>
              <EventFilterChips
                activeFilter={activeFilter}
                counts={typeCounts}
                errorCount={errorCount}
                onChange={setActiveFilter}
              />
              <label className="flex items-center gap-1.5 text-meta text-muted-foreground cursor-pointer whitespace-nowrap">
                <Switch
                  checked={groupRepeats}
                  onCheckedChange={setGroupRepeats}
                  className="scale-75"
                />
                {strings.events.groupRepeats}
              </label>
            </>
          )}

          <span className="text-meta text-muted-foreground tabular-nums ml-auto inline-flex items-center gap-1.5 whitespace-nowrap">
            {!hasEvents && streaming && (
              <Activity className="h-3 w-3 opacity-50 animate-pulse" />
            )}
            {pausedEvents
              ? `paused · +${Math.max(0, events.length - pausedEvents.length)} new`
              : hasEvents
                ? `${countLabel} ${strings.events.countSuffix}${streaming ? ` · ${strings.events.live}` : ''}`
                : streaming
                  ? strings.events.waiting
                  : `0 ${strings.events.countSuffix}`}
          </span>

          <Button
            variant="ghost"
            size="icon"
            onClick={() => setPausedEvents(pausedEvents ? null : events)}
            title={pausedEvents ? strings.events.resume : strings.events.pause}
            aria-label={pausedEvents ? strings.events.resume : strings.events.pause}
            className={cn('h-6 w-6', pausedEvents && 'text-primary')}
            disabled={!hasEvents && !pausedEvents}
          >
            {pausedEvents ? <Play className="h-3 w-3" /> : <Pause className="h-3 w-3" />}
          </Button>

          {hasEvents && <EventSearchBar query={searchQuery} onChange={setSearchQuery} />}

          <Button
            variant="ghost"
            size="icon"
            onClick={exportLog}
            title={strings.events.export}
            aria-label={strings.events.export}
            className="h-6 w-6"
            disabled={!hasEvents}
          >
            <Download className="h-3 w-3" />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            onClick={() => {
              setEvents([])
              setPausedEvents(null)
              setDetailEvent(null)
              seenRef.current = 0
            }}
            title={strings.events.clear}
            aria-label={strings.events.clear}
            className="h-6 w-6"
            disabled={!hasEvents}
          >
            <Trash2 className="h-3 w-3" />
          </Button>
        </div>

        <div
          className={cn(
            ROW_GRID,
            'px-3 py-1.5 text-[10px] font-semibold tracking-wider uppercase text-muted-foreground border-b border-border surface-inset border-l-[3px] border-l-transparent'
          )}
        >
          <span>Time</span>
          <span>Type</span>
          <span>Method</span>
          <span>Path</span>
          <span>Status</span>
          <span className="text-right">Dur</span>
          <span className="text-right">Count</span>
        </div>

        <div ref={logRef} className="flex-1 overflow-y-auto text-xs">
          {rows.length === 0 && hasEvents && (
            <div className="text-muted-foreground text-center py-4 text-meta">
              No events match the current filter.
            </div>
          )}
          {!hasEvents && streaming && <WaitingHint session={session} />}
          {rows.map(({ entry, count }, i) => (
            <EventRow
              key={`${entry.receivedAt.getTime()}-${i}`}
              parsed={entry.parsed}
              receivedAt={entry.receivedAt}
              count={count}
              durationMs={entry.durationMs}
              selected={detailRow !== null && detailRow.entry === entry}
              onClick={
                entry.parsed.rawData !== undefined
                  ? () => selectRow({ entry, count })
                  : undefined
              }
            />
          ))}
        </div>
    </div>
  )

  if (!detailRow) return <div className="h-full min-h-0">{table}</div>

  return (
    <div className="h-full min-h-0">
      <ResizableSplit
        storageKey={`session-monitor-inspector:${session.session_id}`}
        defaultWidthPercent={68}
        minWidthPercent={40}
        maxWidthPercent={85}
        left={<div className="h-full pr-2">{table}</div>}
        right={
          <div className="h-full pl-2">
            <InspectorPane
              detail={{
                summary: detailRow.entry.parsed.summary,
                raw: toDisplayEvent(detailRow.entry.parsed.rawData),
                position: { current: detailIndex + 1, total: detailableRows.length },
                durationMs: detailRow.entry.durationMs,
              }}
              onClose={() => setDetailEvent(null)}
            />
          </div>
        }
      />
    </div>
  )
}
