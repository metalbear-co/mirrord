import { X } from 'lucide-react'
import { Button, cn } from '@metalbear/ui'
import JsonHighlight from '../JsonHighlight'
import CopyButton from '../CopyButton'
import { strings } from '../../strings'

export interface InspectorDetail {
  summary: string
  raw: unknown
  position: { current: number; total: number }
}

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
    <div className="border border-border rounded-lg overflow-hidden">
      <div className="flex items-center px-3 py-1.5 text-caps text-muted-foreground surface-inset border-b border-border/60">
        <span>{title}</span>
        {trailing && <span className="ml-auto normal-case tracking-normal">{trailing}</span>}
      </div>
      {children}
    </div>
  )
}

export default function InspectorPane({ detail, onClose }: Props) {
  const record = asRecord(detail.raw)
  const headers = asRecord(record?.headers)
  const status = typeof record?.status === 'number' ? record.status : undefined
  const eventType = typeof record?.type === 'string' ? record.type : undefined
  const httpVersion = typeof record?.http_version === 'string' ? record.http_version : undefined
  const rawWithoutHeaders = record
    ? Object.fromEntries(Object.entries(record).filter(([key]) => key !== 'headers'))
    : detail.raw

  return (
    <div className="w-[340px] shrink-0 bg-card border border-border rounded-lg flex flex-col overflow-hidden">
      <div className="flex items-center gap-2 px-4 py-2.5 border-b border-border">
        <span className="text-body font-semibold">{strings.events.inspector}</span>
        <span className="text-[11px] text-muted-foreground/60 tabular-nums">
          {detail.position.current} / {detail.position.total} · {strings.events.detailNavHint}
        </span>
        <Button variant="ghost" size="icon" className="ml-auto h-6 w-6" onClick={onClose} aria-label="Close inspector">
          <X className="h-3.5 w-3.5" />
        </Button>
      </div>
      <div className="flex-1 overflow-y-auto p-4 flex flex-col gap-3">
        <div className="font-mono text-sm font-bold break-words [overflow-wrap:anywhere]">
          {detail.summary}
        </div>
        <div className="flex gap-1.5 flex-wrap">
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
        </div>
        {headers && Object.keys(headers).length > 0 && (
          <SectionCard title={strings.events.inspectorHeaders}>
            <div className="font-mono text-xs leading-relaxed px-3 py-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-0.5">
              {Object.entries(headers).map(([name, value]) => (
                <div key={name} className="contents">
                  <span className="text-primary">{name}</span>
                  <span className="break-all">
                    {Array.isArray(value) ? value.join(', ') : String(value)}
                  </span>
                </div>
              ))}
            </div>
          </SectionCard>
        )}
        <SectionCard
          title={strings.events.inspectorRaw}
          trailing={
            <CopyButton getText={() => JSON.stringify(detail.raw, null, 2)} title={strings.events.copyJson} />
          }
        >
          <div className="px-3 py-2 overflow-x-auto">
            <JsonHighlight value={rawWithoutHeaders} />
          </div>
        </SectionCard>
      </div>
    </div>
  )
}
