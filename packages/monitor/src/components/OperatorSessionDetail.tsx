import { useEffect, useState } from 'react'
import { Badge, Card, CardContent, CardHeader } from '@metalbear/ui'
import {
  Clock,
  FileJson,
  FlaskConical,
  Network,
  Radio,
  User,
} from 'lucide-react'
import type {
  OperatorLockedPort,
  OperatorQueueSplits,
  OperatorSessionSummary,
} from '../types'
import type { ExtensionState } from '../extensionBridge'
import CopyButton from './CopyButton'
import JoinBar from './JoinBar'
import JsonHighlight from './JsonHighlight'
import MetadataStrip from './MetadataStrip'
import Widget from './Widget'

interface OperatorSessionDetailProps {
  session: OperatorSessionSummary
  extensionState: ExtensionState
  onJoin: () => Promise<{ ok: boolean; error?: string }>
  onLeave: () => Promise<{ ok: boolean; error?: string }>
}

function formatUptime(secs: number): string {
  const seconds = Math.max(0, Math.floor(secs))
  const minutes = Math.floor(seconds / 60)
  const hours = Math.floor(minutes / 60)
  if (hours > 0) return `${hours}h ${minutes % 60}m`
  if (minutes > 0) return `${minutes}m ${seconds % 60}s`
  return `${seconds}s`
}

function relativeTime(iso: string): string {
  const t = new Date(iso).getTime()
  if (!Number.isFinite(t)) return ''
  const diff = (Date.now() - t) / 1000
  if (diff < 60) return `${Math.max(0, Math.floor(diff))}s ago`
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`
  return `${Math.floor(diff / 86400)}d ago`
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
  const baseAt = Date.now()
  const [uptime, setUptime] = useState(baseSecs)
  useEffect(() => {
    setUptime(baseSecs)
    const interval = setInterval(() => {
      setUptime(baseSecs + Math.floor((Date.now() - baseAt) / 1000))
    }, 1000)
    return () => clearInterval(interval)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session.id, baseSecs])

  const splitsTotal = totalSplits(splits)

  return (
    <div className="h-full flex flex-col">
      <div className="border-b border-border px-4 py-2 bg-card/30 shrink-0">
        <div className="flex items-center gap-x-3 gap-y-1 flex-wrap">
          <div className="flex items-center gap-2.5 min-w-0">
            <span className="h-2 w-2 rounded-full bg-emerald-500 shrink-0" />
            <span className="font-mono text-sm font-semibold text-foreground truncate">
              {targetLabel}
            </span>
            <Badge
              variant="outline"
              className="text-[9px] px-1.5 py-0 h-4 tracking-wider font-normal text-primary border-primary/40 shrink-0"
            >
              operator
            </Badge>
            {isPreview && (
              <Badge
                variant="outline"
                className="text-[9px] px-1.5 py-0 h-4 tracking-wider font-normal text-emerald-500 border-emerald-500/40 inline-flex items-center gap-1 shrink-0"
              >
                <FlaskConical className="h-2.5 w-2.5" />
                preview
              </Badge>
            )}
          </div>

          <div className="flex items-center gap-x-3 gap-y-1 text-[11px] text-muted-foreground flex-wrap">
            <span className="inline-flex items-center gap-1">
              <Clock className="h-3 w-3" />
              <span className="font-mono tabular-nums">
                {formatUptime(uptime)}
              </span>
            </span>
            <span className="inline-flex items-center gap-1">
              <Network className="h-3 w-3" />
              {lockedPorts.length}{' '}
              {lockedPorts.length === 1 ? 'port' : 'ports'}
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

          <span className="ml-auto text-[10px] text-muted-foreground font-mono uppercase tracking-wider">
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

        <div>
          {(() => {
            const configValue = {
              id: session.id,
              key: session.key,
              namespace: session.namespace,
              target: session.target,
              owner: session.owner,
              createdAt: session.createdAt,
              durationSecs: session.durationSecs,
              lockedPorts: session.lockedPorts ?? [],
              queueSplits: session.queueSplits ?? {
                sqs: 0,
                rabbitmq: 0,
                kafka: 0,
              },
              httpFilter: session.httpFilter ?? null,
            }
            return (
              <Widget
                title="Config"
                icon={<FileJson className="h-3 w-3" />}
                trailing={
                  <CopyButton
                    getText={() => JSON.stringify(configValue, null, 2)}
                    title="Copy config"
                  />
                }
              >
                <div className="p-4">
                  <JsonHighlight value={configValue} />
                </div>
              </Widget>
            )
          })()}
        </div>
      </div>
    </div>
  )
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="grid grid-cols-[110px_1fr] items-baseline gap-3 px-4 py-1.5">
      <span className="text-xs text-muted-foreground">{label}</span>
      <span className="text-xs font-mono font-medium text-foreground break-words">
        {value}
      </span>
    </div>
  )
}

function PortChip({ port }: { port: OperatorLockedPort }) {
  const tooltip = port.filter
    ? `${port.kind} :${port.port} · ${port.filter}`
    : `${port.kind} :${port.port}`
  return (
    <span
      className="inline-flex items-center gap-1.5 rounded-full border border-border bg-card/40 px-2 py-0.5 text-[11px] font-mono"
      title={tooltip}
    >
      <span className="text-muted-foreground uppercase tracking-wider text-[9.5px]">
        {port.kind}
      </span>
      <span className="text-foreground font-medium">:{port.port}</span>
      {port.filter && (
        <span className="text-muted-foreground/70 max-w-[120px] truncate">
          {port.filter}
        </span>
      )}
    </span>
  )
}
