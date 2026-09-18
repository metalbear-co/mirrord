import { memo, useEffect, useMemo, useState } from 'react'
import {
  Badge,
  Button,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  cn,
} from '@metalbear/ui'
import { ArrowDown, ArrowUp, Trash2 } from 'lucide-react'
import { labelForType, type SubscribeEventRow } from '../subscribeEvents'
import type { Routing } from '../eventsStore'
import { strings } from '../strings'
import { formatTime24 } from './events/parseEvent'
import LiveDot from './LiveDot'

// Radix rejects an empty option value.
const ALL_TYPES = '__all__'

// Each categorical column carries its own hue, so one broker or every 5xx stands out unread.
const TONES = {
  sky: 'border-sky-500/30 bg-sky-500/10 text-sky-700 dark:text-sky-300',
  violet:
    'border-violet-500/30 bg-violet-500/10 text-violet-700 dark:text-violet-300',
  orange:
    'border-orange-500/30 bg-orange-500/10 text-orange-700 dark:text-orange-300',
  rose: 'border-rose-500/30 bg-rose-500/10 text-rose-700 dark:text-rose-300',
  cyan: 'border-cyan-500/30 bg-cyan-500/10 text-cyan-700 dark:text-cyan-300',
  amber:
    'border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300',
  emerald:
    'border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300',
  teal: 'border-teal-500/30 bg-teal-500/10 text-teal-700 dark:text-teal-300',
  red: 'border-red-500/30 bg-red-500/10 text-red-700 dark:text-red-300',
  neutral: 'border-border bg-muted/40 text-muted-foreground',
} as const

const TYPE_TONES: Record<string, keyof typeof TONES> = {
  http: 'sky',
  http_response: 'sky',
  sqs: 'violet',
  kafka_message: 'orange',
  rmq: 'rose',
  gcppubsub: 'cyan',
  azure_service_bus: 'teal',
  bullmq: 'amber',
  nats: 'emerald',
  natspubsub: 'emerald',
  redis_message: 'red',
  lagged: 'neutral',
}

const ROUTING_TONES: Record<string, keyof typeof TONES> = {
  stolen: 'emerald',
  mirrored: 'sky',
  filtered: 'neutral',
}

const HTTP_SERVER_ERROR = 500
const HTTP_CLIENT_ERROR = 400
const HTTP_REDIRECT = 300

/** Tone of a status: a routing outcome, or an HTTP status code by its class. */
function statusTone(status: string): keyof typeof TONES {
  const routing = ROUTING_TONES[status]
  if (routing) return routing

  const code = Number(status)
  if (!Number.isFinite(code)) return 'neutral'
  if (code >= HTTP_SERVER_ERROR) return 'red'
  if (code >= HTTP_CLIENT_ERROR) return 'amber'
  if (code >= HTTP_REDIRECT) return 'sky'
  return 'emerald'
}

interface Props {
  rows: SubscribeEventRow[]
  streaming: boolean
  routing: Routing
  onRoutingChange: (routing: Routing) => void
  onClear: () => void
}

function formatTimestamp(timestamp: string): string {
  const parsed = new Date(timestamp)
  return Number.isFinite(parsed.getTime()) ? formatTime24(parsed) : timestamp
}

function Cell({ value, mono }: { value: string; mono?: boolean }) {
  return (
    <TableCell
      className={cn(
        'truncate',
        mono === true && 'font-mono',
        value === '' && 'text-muted-foreground',
      )}
      title={value === '' ? undefined : value}
    >
      {value === '' ? strings.subscribeEvents.emptyCell : value}
    </TableCell>
  )
}

function EventRow({ row }: { row: SubscribeEventRow }) {
  return (
    <TableRow>
      <Cell value={formatTimestamp(row.timestamp)} />
      <Cell value={row.sessionKey} mono />
      <Cell value={row.serviceName} />
      <TableCell>
        <Badge
          variant="outline"
          className={TONES[TYPE_TONES[row.type] ?? 'neutral']}
        >
          {labelForType(row.type) ?? row.type}
        </Badge>
      </TableCell>
      <Cell value={row.source} mono />
      <TableCell>
        {row.status === '' ? (
          <span className="text-muted-foreground">
            {strings.subscribeEvents.emptyCell}
          </span>
        ) : (
          <Badge variant="outline" className={TONES[statusTone(row.status)]}>
            {row.status}
          </Badge>
        )}
      </TableCell>
    </TableRow>
  )
}

// Memoized so a frame that appends rows re-renders only those.
const MemoEventRow = memo(EventRow)

export default function SubscribeEventsTable({
  rows,
  streaming,
  routing,
  onRoutingChange,
  onClear,
}: Props) {
  const [query, setQuery] = useState('')
  const [typeFilter, setTypeFilter] = useState(ALL_TYPES)
  const [newestFirst, setNewestFirst] = useState(true)

  // Exactly the types present in the buffer, so an event type this build has never heard of is
  // still filterable.
  const types = useMemo(
    () =>
      [...new Set(rows.map((row) => row.type))]
        .map((type) => [type, labelForType(type) ?? type] as const)
        .sort(([, a], [, b]) => a.localeCompare(b)),
    [rows],
  )

  // A type that ages out of the buffer must not leave the table filtered to nothing, nor re-engage
  // the filter if it comes back.
  const typeKnown =
    typeFilter === ALL_TYPES || types.some(([type]) => type === typeFilter)
  useEffect(() => {
    if (!typeKnown) setTypeFilter(ALL_TYPES)
  }, [typeKnown])
  const activeType = typeKnown ? typeFilter : ALL_TYPES

  const visible = useMemo(() => {
    const text = query.trim().toLowerCase()
    const matches = (row: SubscribeEventRow) =>
      row.sessionKey.toLowerCase().includes(text) ||
      row.serviceName.toLowerCase().includes(text) ||
      row.source.toLowerCase().includes(text)

    const matching = rows.filter(
      (row) =>
        (routing === 'both' ||
          (routing === 'filtered' ? row.filtered : !row.filtered)) &&
        (text === '' || matches(row)) &&
        (activeType === ALL_TYPES || row.type === activeType),
    )

    return newestFirst ? matching.reverse() : matching
  }, [rows, query, activeType, newestFirst, routing])

  return (
    <div className="flex h-full flex-col">
      <div className="border-border flex items-center gap-2 border-b px-3 py-2">
        <Input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={strings.subscribeEvents.filterPlaceholder}
          className="h-8 w-[260px]"
        />
        <Select value={activeType} onValueChange={setTypeFilter}>
          <SelectTrigger className="h-8 w-[200px]">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={ALL_TYPES}>
              {strings.subscribeEvents.allTypes}
            </SelectItem>
            {types.map(([type, label]) => (
              <SelectItem key={type} value={type}>
                {label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select
          value={routing}
          onValueChange={(value) => onRoutingChange(value as Routing)}
        >
          <SelectTrigger
            className="h-8 w-[190px]"
            title={strings.subscribeEvents.routingHint}
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="consumed">
              {strings.subscribeEvents.routingConsumed}
            </SelectItem>
            <SelectItem value="filtered">
              {strings.subscribeEvents.routingFiltered}
            </SelectItem>
            <SelectItem value="both">
              {strings.subscribeEvents.routingBoth}
            </SelectItem>
          </SelectContent>
        </Select>
        <span className="text-muted-foreground text-meta ml-auto flex items-center gap-1.5 tabular-nums">
          <LiveDot active={streaming} />
          {rows.length} {strings.events.countSuffix}
          {visible.length !== rows.length && (
            <span>
              · {visible.length} {strings.subscribeEvents.shownSuffix}
            </span>
          )}
        </span>
        <Button
          variant="ghost"
          size="sm"
          onClick={onClear}
          title={strings.events.clear}
          aria-label={strings.events.clear}
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>

      <div className="min-h-0 flex-1 overflow-auto">
        <table className="w-full table-fixed caption-bottom text-sm">
          <colgroup>
            <col className="w-[11%]" />
            <col className="w-[14%]" />
            <col className="w-[15%]" />
            <col className="w-[16%]" />
            <col className="w-[32%]" />
            <col className="w-[12%]" />
          </colgroup>
          <TableHeader className="bg-background sticky top-0 z-10">
            <TableRow>
              <TableHead>
                <button
                  type="button"
                  onClick={() => setNewestFirst((previous) => !previous)}
                  title={
                    newestFirst
                      ? strings.subscribeEvents.sortNewestFirst
                      : strings.subscribeEvents.sortOldestFirst
                  }
                  className="hover:text-foreground inline-flex items-center gap-1 transition-colors"
                >
                  {strings.subscribeEvents.time}
                  {newestFirst ? (
                    <ArrowDown className="h-3 w-3" />
                  ) : (
                    <ArrowUp className="h-3 w-3" />
                  )}
                </button>
              </TableHead>
              <TableHead>{strings.subscribeEvents.sessionKey}</TableHead>
              <TableHead>{strings.subscribeEvents.service}</TableHead>
              <TableHead>{strings.subscribeEvents.type}</TableHead>
              <TableHead>{strings.subscribeEvents.source}</TableHead>
              <TableHead>{strings.subscribeEvents.status}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {visible.map((row) => (
              <MemoEventRow key={row.seq} row={row} />
            ))}
          </TableBody>
        </table>

        {visible.length === 0 && (
          <div className="text-muted-foreground text-body px-3 py-6 text-center">
            {rows.length === 0
              ? strings.events.waiting
              : strings.events.noFilterMatch}
          </div>
        )}
      </div>
    </div>
  )
}
