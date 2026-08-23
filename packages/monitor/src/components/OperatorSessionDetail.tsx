import { useEffect, useState } from 'react'
import { Badge } from '@metalbear/ui'
import { Clock, FlaskConical, Network, Radio, User } from 'lucide-react'
import type { OperatorQueueSplits, OperatorSessionSummary } from '../types'
import type { ExtensionState } from '../extensionBridge'
import { strings } from '../strings'
import {
  NOT_SET,
  formatQueueSplits,
  isPreviewSession,
  previewPhaseLabel,
  previewPhaseTone,
  type PreviewTone,
} from '../utils'
import JoinBar from './JoinBar'
import MetadataStrip from './MetadataStrip'
import PortModeChip from './PortModeChip'

const SECS_PER_MIN = 60
const MINS_PER_HOUR = 60
const MS_PER_SEC = 1000
const UPTIME_TICK_MS = 1000

// Matches the badge in the sidebar: green only while a deployment is actually backing the preview.
const STATUS_DOT_TONE: Record<PreviewTone, string> = {
  live: 'bg-emerald-500',
  pending: 'bg-amber-500',
  idle: 'bg-muted-foreground',
  failed: 'bg-destructive',
}

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

// Own-session equivalent lives in `utils.extractHttpFilterSummary`, reading the same fields
// (header/path/all_of/any_of) off the local config instead of the operator's session status.
function describeFilter(
  f: OperatorSessionSummary['httpFilter'],
): string | null {
  if (!f) return null
  if (f.headerFilter) return `header: ${f.headerFilter}`
  if (f.pathFilter) return `path: ${f.pathFilter}`
  if (f.allOf?.length) return `${f.allOf.length} filters (all)`
  if (f.anyOf?.length) return `${f.anyOf.length} filters (any)`
  return null
}

function totalSplits(s: OperatorQueueSplits | undefined): number {
  if (!s) return 0
  return s.sqs + s.rabbitmq + s.kafka
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
  const isPreview = isPreviewSession(session)
  const preview = session.preview
  const phaseLabel = preview ? previewPhaseLabel(preview) : null
  // Only a preview reports a phase; everything else here is a running exec session.
  const tone = preview ? previewPhaseTone(preview) : 'live'

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
    <div className="flex h-full flex-col">
      <div className="border-border surface-inset shrink-0 border-b px-4 py-2">
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <div className="flex min-w-0 items-center gap-2">
            <span
              className={`h-2 w-2 shrink-0 rounded-full ${STATUS_DOT_TONE[tone]}`}
            />
            <span className="text-title text-foreground truncate font-mono">
              {targetLabel}
            </span>
            <Badge
              variant="outline"
              style={{ fontSize: 10 }}
              className="text-muted-foreground border-border h-4 shrink-0 px-1.5 py-0 font-medium"
            >
              {strings.operatorDetail.operatorBadge}
            </Badge>
            {isPreview && (
              <Badge
                variant="outline"
                style={{ fontSize: 10 }}
                className={`inline-flex h-4 shrink-0 items-center gap-1 px-1.5 py-0 font-medium ${
                  tone === 'failed'
                    ? 'text-destructive border-destructive/40'
                    : 'text-muted-foreground border-border'
                }`}
              >
                <FlaskConical className="h-2.5 w-2.5" />
                {phaseLabel ?? strings.operatorDetail.previewBadge}
              </Badge>
            )}
          </div>

          <div className="text-meta text-muted-foreground flex flex-wrap items-center gap-x-3 gap-y-1">
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

          <span className="text-caps text-muted-foreground ml-auto font-mono">
            {strings.operatorDetail.readOnly}
          </span>
        </div>
      </div>

      <div className="mx-auto flex min-h-0 w-full max-w-7xl flex-1 flex-col gap-4 p-4">
        <JoinBar
          joinKey={session.key}
          extensionState={extensionState}
          onJoin={onJoin}
          onLeave={onLeave}
        />

        <MetadataStrip
          items={[
            {
              label: 'Key',
              value: session.key,
            },
            { label: 'Session ID', value: session.id },
            {
              label: lockedPorts.length === 1 ? 'Port' : 'Ports',
              value:
                lockedPorts.length > 0 ? (
                  <span className="inline-flex flex-wrap items-center gap-1.5">
                    {lockedPorts.map((p) => (
                      <PortModeChip
                        key={`${p.kind}:${p.port}:${p.filter ?? ''}`}
                        port={p.port}
                        mode={p.kind}
                        filter={p.filter}
                      />
                    ))}
                  </span>
                ) : (
                  NOT_SET
                ),
            },
            {
              label: 'Mode',
              value:
                lockedPorts.length > 0
                  ? Array.from(new Set(lockedPorts.map((p) => p.kind))).join(
                      ' · ',
                    )
                  : NOT_SET,
            },
            { label: 'Namespace', value: session.namespace || NOT_SET },
            {
              label: 'Container',
              value: session.target?.container ?? NOT_SET,
            },
            {
              // Preview-env sessions don't carry a meaningful HTTP filter of their own.
              label: 'HTTP filter',
              value: isPreview
                ? NOT_SET
                : (describeFilter(session.httpFilter) ?? NOT_SET),
            },
            {
              label: 'Queue splits',
              value:
                splitsTotal > 0
                  ? formatQueueSplits(
                      splits ?? { sqs: 0, rabbitmq: 0, kafka: 0 },
                    )
                  : NOT_SET,
            },
          ]}
        />
      </div>
    </div>
  )
}
