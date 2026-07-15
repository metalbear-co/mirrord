import { useEffect, useState } from 'react'
import { Badge } from '@metalbear/ui'
import { Clock, FlaskConical, Network, Radio, User } from 'lucide-react'
import type {
  OperatorLockedPort,
  OperatorQueueSplits,
  OperatorSessionSummary,
} from '../types'
import type { ExtensionState } from '../extensionBridge'
import JoinBar from './JoinBar'
import MetadataStrip from './MetadataStrip'

const SECS_PER_MIN = 60
const MINS_PER_HOUR = 60
const MS_PER_SEC = 1000
const UPTIME_TICK_MS = 1000

interface OperatorSessionDetailProps {
  session: OperatorSessionSummary
  extensionState: ExtensionState
  onJoin: () => Promise<{ ok: boolean; error?: string | undefined }>
  onLeave: () => Promise<{ ok: boolean; error?: string | undefined }>
}

function formatUptime(secs: number): string {
  const seconds = Math.max(0, Math.floor(secs))
  const minutes = Math.floor(seconds / SECS_PER_MIN)
  const hours = Math.floor(minutes / MINS_PER_HOUR)
  if (hours > 0) return `${hours}h ${minutes % MINS_PER_HOUR}m`
  if (minutes > 0) return `${minutes}m ${seconds % SECS_PER_MIN}s`
  return `${seconds}s`
}

function describeFilter(f: OperatorSessionSummary['httpFilter']): string {
  if (!f) return 'no filter'
  if (f.headerFilter) return `header: ${f.headerFilter}`
  if (f.pathFilter) return `path: ${f.pathFilter}`
  if (f.allOf?.length) return `${f.allOf.length} filters (all)`
  if (f.anyOf?.length) return `${f.anyOf.length} filters (any)`
  return 'no filter'
}

function totalSplits(s: OperatorQueueSplits | undefined): number {
  if (!s) return 0
  return s.sqs + s.rabbitmq + s.kafka
}

function splitSummary(s: OperatorQueueSplits | undefined): string {
  if (!s) return ''
  const parts: string[] = []
  if (s.sqs > 0) parts.push(`SQS ${s.sqs}`)
  if (s.rabbitmq > 0) parts.push(`RabbitMQ ${s.rabbitmq}`)
  if (s.kafka > 0) parts.push(`Kafka ${s.kafka}`)
  return parts.join(' · ')
}

export default function OperatorSessionDetail({
  session,
  extensionState,
  onJoin,
  onLeave,
}: OperatorSessionDetailProps) {
  const targetLabel = session.target
    ? `${session.target.kind}/${session.target.name}`
    : 'targetless'
  const lockedPorts = session.lockedPorts ?? []
  const splits = session.queueSplits
  const isPreview = session.owner.username === 'preview-env'

  const baseSecs = session.durationSecs ?? 0
  const [uptime, setUptime] = useState(baseSecs)
  useEffect(() => {
    setUptime(baseSecs)
    const startedAt = Date.now()
    const interval = setInterval(() => {
      setUptime(baseSecs + Math.floor((Date.now() - startedAt) / MS_PER_SEC))
    }, UPTIME_TICK_MS)
    return () => clearInterval(interval)
  }, [session.id, baseSecs])

  const splitsTotal = totalSplits(splits)

  return (
    <div className="h-full flex flex-col">
      <div className="border-b border-border px-4 py-2 surface-inset shrink-0">
        <div className="flex items-center gap-x-3 gap-y-1 flex-wrap">
          <div className="flex items-center gap-2 min-w-0">
            <span className="h-2 w-2 rounded-full bg-emerald-500 shrink-0" />
            <span className="font-mono text-title text-foreground truncate">
              {targetLabel}
            </span>
            <Badge
              variant="outline"
              style={{ fontSize: 10 }}
              className="px-1.5 py-0 h-4 font-medium text-muted-foreground border-border shrink-0"
            >
              operator
            </Badge>
            {isPreview && (
              <Badge
                variant="outline"
                style={{ fontSize: 10 }}
                className="px-1.5 py-0 h-4 font-medium text-muted-foreground border-border inline-flex items-center gap-1 shrink-0"
              >
                <FlaskConical className="h-2.5 w-2.5" />
                preview
              </Badge>
            )}
          </div>

          <div className="flex items-center gap-x-3 gap-y-1 text-meta text-muted-foreground flex-wrap">
            <span className="inline-flex items-center gap-1">
              <Clock className="h-3 w-3" />
              <span className="font-mono tabular-nums">
                {formatUptime(uptime)}
              </span>
            </span>
            <span className="inline-flex items-center gap-1">
              <Network className="h-3 w-3" />
              {lockedPorts.length} {lockedPorts.length === 1 ? 'port' : 'ports'}
            </span>
            <span className="inline-flex items-center gap-1">
              <Radio className="h-3 w-3" />
              {splitsTotal} {splitsTotal === 1 ? 'split' : 'splits'}
            </span>
            <span
              className="inline-flex items-center gap-1 truncate"
              title={session.owner.k8sUsername}
            >
              <User className="h-3 w-3" />
              {session.owner.username}
            </span>
          </div>

          <span className="ml-auto text-caps text-muted-foreground font-mono">
            read-only
          </span>
        </div>
      </div>

      <div className="flex-1 min-h-0 flex flex-col p-4 gap-4 max-w-7xl mx-auto w-full">
        <JoinBar
          joinKey={session.key}
          extensionState={extensionState}
          onJoin={onJoin}
          onLeave={onLeave}
        />

        <MetadataStrip
          items={[
            { label: 'Namespace', value: session.namespace || '—' },
            { label: 'Session ID', value: session.id },
            { label: 'Key', value: session.key },
            ...(session.target?.container
              ? [{ label: 'Container', value: session.target.container }]
              : []),
            ...(isPreview
              ? []
              : [
                  {
                    label: 'HTTP filter',
                    value: describeFilter(session.httpFilter),
                  },
                ]),
            ...(lockedPorts.length > 0
              ? [
                  {
                    label:
                      lockedPorts.length === 1 ? 'Locked port' : 'Locked ports',
                    value: (
                      <span className="inline-flex flex-wrap items-center gap-1.5">
                        {lockedPorts.map((p, i) => (
                          <PortChip key={`${p.port}-${i}`} port={p} />
                        ))}
                      </span>
                    ),
                  },
                ]
              : []),
            ...(splitsTotal > 0
              ? [{ label: 'Queue splits', value: splitSummary(splits) }]
              : []),
          ]}
        />
      </div>
    </div>
  )
}

function PortChip({ port }: { port: OperatorLockedPort }) {
  const tooltip = port.filter
    ? `${port.kind} :${port.port} · ${port.filter}`
    : `${port.kind} :${port.port}`
  return (
    <span
      className="inline-flex items-center gap-1.5 rounded-full border border-border bg-card/40 px-2 py-0.5 text-meta font-mono"
      title={tooltip}
    >
      <span className="text-muted-foreground text-caps">{port.kind}</span>
      <span className="text-foreground font-medium">:{port.port}</span>
      {port.filter && (
        <span className="text-muted-foreground/70 max-w-[120px] truncate">
          {port.filter}
        </span>
      )}
    </span>
  )
}
