import { Fragment, useState } from 'react'
import { X } from 'lucide-react'
import { Button, cn } from '@metalbear/ui'
import JsonHighlight from '../JsonHighlight'
import BreakableText from '../BreakableText'
import { formatBytes, formatTime24 } from './parseEvent'
import { strings } from '../../strings'

// Long request titles are URLs, which have no spaces to wrap at. Rather than break mid-token
// (…/order↵s), insert soft break opportunities after URL delimiters so a wrapped title only
// ever breaks at a slash, dot, or query separator — reading as intentional segments.
function withUrlBreaks(text: string): React.ReactNode[] {
  return text.split(/(?<=[/.?&=])/).map((part, i) => (
    <Fragment key={i}>
      {part}
      <wbr />
    </Fragment>
  ))
}

export interface InspectorDetail {
  summary: string
  raw: unknown
  position: { current: number; total: number }
  durationMs?: number
  occurrences?: { count: number; first: Date; last: Date }
}

// One sentence per event kind explaining what mirrord actually did — the event feed is the
// first place users see the cluster acting on their behalf, so say so at the point of curiosity.
const CONTEXT_NOTES: Record<string, string> = {
  dns_query:
    'Resolved inside the cluster by the mirrord agent, exactly as the target pod would resolve it.',
  outgoing_connection:
    "Opened by your local process and routed through the target pod's network.",
  file_op: "Executed against the target pod's filesystem by the mirrord agent.",
  layer_connected: 'Your local process, hooked by mirrord and attached to this session.',
  layer_disconnected: 'The hooked local process exited or detached from this session.',
}

const FD_ONLY_NOTE =
  'No path on this operation: it acts on a file descriptor from an earlier open (see the preceding open event).'

interface Props {
  detail: InspectorDetail
  onClose: () => void
}

const asRecord = (value: unknown): Record<string, unknown> | null =>
  value && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : null

const STATUS_TONE = (status: number) =>
  status >= 400
    ? 'text-red-700 bg-red-100 dark:text-red-400 dark:bg-red-950'
    : 'text-emerald-700 bg-emerald-100 dark:text-emerald-400 dark:bg-emerald-950'

// Rebuilds a runnable curl command from a captured request event: URL from host/port/path,
// captured headers minus content-length (curl recomputes it), and the merged body if one was
// captured. Only request events carry enough to reproduce the call.
export function buildCurl(raw: unknown): string | null {
  const record = asRecord(raw)
  if (!record || record.type !== 'incoming_request') return null
  const host = typeof record.host === 'string' ? record.host : null
  if (!host) return null
  const method = String(record.method ?? 'GET')
  const port = typeof record.port === 'number' ? record.port : undefined
  const scheme = port === 443 ? 'https' : 'http'
  const needsPort = port !== undefined && port !== 80 && port !== 443 && !host.includes(':')
  const url = `${scheme}://${host}${needsPort ? `:${port}` : ''}${String(record.path ?? '/')}`
  const esc = (value: string) => value.replace(/'/g, `'\\''`)

  const lines = [`curl -X ${method} '${esc(url)}'`]
  for (const [name, value] of Object.entries(asRecord(record.headers) ?? {})) {
    if (name.toLowerCase() === 'content-length') continue
    const values = Array.isArray(value) ? value : [value]
    for (const v of values) lines.push(`  -H '${esc(`${name}: ${String(v)}`)}'`)
  }
  const body = record.body
  if (typeof body === 'string' && body.length > 0) {
    lines.push(`  --data-raw '${esc(body)}'`)
  } else if (body && typeof body === 'object') {
    lines.push(`  --data-raw '${esc(JSON.stringify(body))}'`)
  }
  return lines.join(' \\\n')
}

function SectionCard({
  title,
  trailing,
  children,
}: {
  title: string
  trailing?: React.ReactNode
  children: React.ReactNode
}) {
  return (
    <div className="border border-border rounded-lg overflow-hidden shrink-0">
      <div className="flex items-center px-3 py-1.5 text-[10px] font-semibold tracking-wider uppercase text-muted-foreground surface-inset border-b border-border/60">
        <span>{title}</span>
        {trailing && <span className="ml-auto normal-case tracking-normal">{trailing}</span>}
      </div>
      <div className="max-h-56 overflow-auto">{children}</div>
    </div>
  )
}

// Small labeled copy button with transient feedback, shared by the cURL and JSON actions.
function CopyTextButton({
  label,
  getText,
  title,
}: {
  label: string
  getText: () => string
  title?: string
}) {
  const [copied, setCopied] = useState(false)
  return (
    <Button
      variant="outline"
      size="sm"
      className="h-6 px-2 text-[11px]"
      title={title}
      onClick={() => {
        navigator.clipboard.writeText(getText())
        setCopied(true)
        setTimeout(() => setCopied(false), 1500)
      }}
    >
      {copied ? strings.events.copied : label}
    </Button>
  )
}

export default function InspectorPane({ detail, onClose }: Props) {
  const record = asRecord(detail.raw)
  const headers = asRecord(record?.headers)
  const status = typeof record?.status === 'number' ? record.status : undefined
  const eventType = typeof record?.type === 'string' ? record.type : undefined
  const httpVersion = typeof record?.http_version === 'string' ? record.http_version : undefined
  const body = record?.body
  const bodyBytes = typeof record?.bytes === 'number' ? record.bytes : undefined
  const bodyTruncated = record?.truncated === true
  const curl = buildCurl(detail.raw)

  // Non-HTTP events (DNS, file ops, outgoing connections, process lifecycle) have no headers or
  // body — show their fields as a key/value grid so the pane is never empty for them. String
  // arrays (a process's cmdline) join into one line rather than being dropped.
  const detailRows =
    record && !headers && body === undefined
      ? Object.entries(record).flatMap(([key, value]): [string, string][] => {
          if (key === 'type' || value === null || value === undefined) return []
          if (typeof value === 'string' || typeof value === 'number' || typeof value === 'boolean') {
            return [[key, String(value)]]
          }
          if (Array.isArray(value) && value.every((v) => typeof v === 'string')) {
            return [[key, value.join(' ')]]
          }
          return []
        })
      : []

  const contextNote =
    eventType === 'file_op' && record?.path == null
      ? `${CONTEXT_NOTES.file_op} ${FD_ONLY_NOTE}`
      : eventType
        ? CONTEXT_NOTES[eventType]
        : undefined
  const occurrences = detail.occurrences

  return (
    <div className="h-full w-full bg-card border border-border rounded-lg flex flex-col overflow-hidden">
      <div className="flex items-center gap-2 px-4 py-2.5 border-b border-border shrink-0">
        <span className="text-body font-semibold">{strings.events.inspector}</span>
        <span className="text-[11px] text-muted-foreground/60 tabular-nums">
          {detail.position.current} / {detail.position.total}
        </span>
        <Button
          variant="ghost"
          size="icon"
          className="ml-auto h-6 w-6"
          onClick={onClose}
          aria-label="Close inspector"
          title="Close (Esc)"
        >
          <X className="h-3.5 w-3.5" />
        </Button>
      </div>
      <div className="flex-1 min-h-0 overflow-y-auto p-4 flex flex-col gap-3">
        <div className="font-mono text-sm font-bold [overflow-wrap:break-word] shrink-0">
          {withUrlBreaks(detail.summary)}
        </div>
        <div className="flex items-center gap-1.5 flex-wrap shrink-0">
          {status !== undefined && (
            <span className={cn('text-[11px] font-semibold rounded-full px-2.5 py-0.5 tabular-nums', STATUS_TONE(status))}>
              {status}
            </span>
          )}
          {eventType && (
            <span className="text-[11px] font-semibold rounded-full px-2.5 py-0.5 bg-primary/15 text-foreground">
              {eventType}
            </span>
          )}
          {httpVersion && (
            <span className="text-[11px] font-semibold rounded-full px-2.5 py-0.5 border border-border text-muted-foreground">
              {httpVersion}
            </span>
          )}
          {detail.durationMs !== undefined && (
            <span className="text-[11px] font-semibold rounded-full px-2.5 py-0.5 border border-border text-muted-foreground tabular-nums">
              {detail.durationMs} ms
            </span>
          )}
          <span className="ml-auto inline-flex gap-1.5">
            {curl && (
              <CopyTextButton label={strings.events.copyCurl} getText={() => curl} title="Copy as cURL (Y)" />
            )}
            <CopyTextButton
              label={strings.events.copyJson}
              getText={() => JSON.stringify(detail.raw, null, 2)}
            />
          </span>
        </div>
        {(contextNote || (occurrences && occurrences.count > 1)) && (
          <div className="flex flex-col gap-1 shrink-0 text-meta text-muted-foreground">
            {occurrences && occurrences.count > 1 && (
              <span className="tabular-nums">
                Seen ×{occurrences.count} in this feed · first {formatTime24(occurrences.first)} ·
                last {formatTime24(occurrences.last)}
              </span>
            )}
            {contextNote && <span>{contextNote}</span>}
          </div>
        )}
        {detailRows.length > 0 && (
          <SectionCard title={strings.events.inspectorDetails}>
            <div className="font-mono text-xs leading-relaxed px-3 py-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-0.5">
              {detailRows.map(([name, value]) => (
                <div key={name} className="contents">
                  <span className="text-primary">{name}</span>
                  <BreakableText text={String(value)} />
                </div>
              ))}
            </div>
          </SectionCard>
        )}
        {headers && Object.keys(headers).length > 0 && (
          <SectionCard title={strings.events.inspectorHeaders}>
            <div className="font-mono text-xs leading-relaxed px-3 py-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-0.5">
              {Object.entries(headers).map(([name, value]) => (
                <div key={name} className="contents">
                  <span className="text-primary">{name}</span>
                  <BreakableText
                    text={Array.isArray(value) ? value.join(', ') : String(value)}
                  />
                </div>
              ))}
            </div>
          </SectionCard>
        )}
        {body !== undefined && (
          <SectionCard
            title={strings.events.inspectorBody}
            trailing={
              bodyBytes !== undefined ? (
                <span className="text-muted-foreground tabular-nums">
                  {formatBytes(bodyBytes)}
                  {bodyTruncated ? ' · truncated' : ''}
                </span>
              ) : undefined
            }
          >
            {typeof body === 'string' ? (
              <pre className="m-0 px-3 py-2 font-mono text-xs whitespace-pre-wrap [overflow-wrap:anywhere]">
                {body}
              </pre>
            ) : (
              <div className="px-3 py-2">
                <JsonHighlight value={body} />
              </div>
            )}
          </SectionCard>
        )}
      </div>
    </div>
  )
}
