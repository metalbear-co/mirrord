import { useState, useEffect } from 'react'
import { SlidersHorizontal, X, ChevronRight, ChevronDown } from 'lucide-react'
import { Button, cn } from '@metalbear/ui'
import JsonHighlight from './JsonHighlight'
import CopyButton from './CopyButton'
import type { PortSubscription } from '../types'

interface Props {
  config: Record<string, unknown>
  sessionKey?: string | null
  portSubs: PortSubscription[]
  onClose: () => void
}

const asRecord = (value: unknown): Record<string, unknown> | null =>
  value && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : null

const asString = (value: unknown): string | null => (typeof value === 'string' ? value : null)

// The config arrives as the diff against defaults, so any of these sections may be absent;
// each card renders only when its data exists and the raw JSON stays available below.
function summarize(config: Record<string, unknown>, portSubs: PortSubscription[]) {
  const target = asRecord(config.target)
  const targetPath = asRecord(target?.path)
  const workload = targetPath
    ? Object.entries(targetPath).find(([, v]) => typeof v === 'string')
    : null
  const feature = asRecord(config.feature)
  const incoming = asRecord(asRecord(feature?.network)?.incoming)
  const modes = Array.from(new Set(portSubs.map((p) => p.mode)))
  const ports = portSubs.map((p) => `:${p.port}`).join(' ')
  const mode =
    asString(incoming?.mode) ?? (modes.length > 0 ? modes.join(' · ') : null)
  const httpFilter = asRecord(incoming?.http_filter)
  const envUnset = asRecord(feature?.env)?.unset
  const fsMapping = asRecord(asRecord(feature?.fs)?.mapping)

  return {
    target: {
      workload: workload ? `${workload[0]}/${workload[1]}` : null,
      namespace: asString(target?.namespace),
      mode: mode ? `${mode}${ports ? ` · port ${ports}` : ''}` : null,
      httpFilter: httpFilter ? Object.entries(httpFilter).map(([k, v]) => `${k}: ${String(v)}`) : null,
    },
    envUnset: Array.isArray(envUnset) ? envUnset.map(String) : null,
    fsMapping: fsMapping ? Object.entries(fsMapping).map(([from, to]) => ({ from, to: String(to) })) : null,
    proxyLogs: {
      external: asString(asRecord(config.external_proxy)?.log_destination),
      internal: asString(asRecord(config.internal_proxy)?.log_destination),
    },
  }
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="border border-border rounded-lg overflow-hidden">
      <div className="px-3.5 py-1.5 text-caps text-muted-foreground surface-inset border-b border-border/60">
        {title}
      </div>
      {children}
    </div>
  )
}

function KeyValueGrid({ rows }: { rows: [string, React.ReactNode][] }) {
  return (
    <div className="px-3.5 py-2.5 text-xs grid grid-cols-[88px_minmax(0,1fr)] gap-y-1.5 items-baseline">
      {rows.map(([label, value]) => (
        <div key={label} className="contents">
          <span className="text-muted-foreground">{label}</span>
          <span className="font-mono break-all">{value}</span>
        </div>
      ))}
    </div>
  )
}

export default function ConfigDrawer({ config, sessionKey, portSubs, onClose }: Props) {
  const [rawOpen, setRawOpen] = useState(false)

  // Capture phase so the drawer wins over the event stream's own Escape handling: pressing
  // Escape with the drawer open should close only the drawer, not the inspector beneath it.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation()
        onClose()
      }
    }
    window.addEventListener('keydown', onKeyDown, { capture: true })
    return () => window.removeEventListener('keydown', onKeyDown, { capture: true })
  }, [onClose])
  const summary = summarize(config, portSubs)
  const rawJson = JSON.stringify(config, null, 2)
  const rawLines = rawJson.split('\n').length

  const targetRows: [string, React.ReactNode][] = []
  if (summary.target.workload) targetRows.push(['workload', summary.target.workload])
  if (summary.target.namespace) targetRows.push(['namespace', summary.target.namespace])
  if (summary.target.mode) targetRows.push(['mode', summary.target.mode])
  if (summary.target.httpFilter)
    targetRows.push(['http filter', summary.target.httpFilter.join(' · ')])

  const proxyRows: [string, React.ReactNode][] = []
  if (summary.proxyLogs.external)
    proxyRows.push(['external', <span key="e" className="truncate block">{summary.proxyLogs.external}</span>])
  if (summary.proxyLogs.internal)
    proxyRows.push(['internal', <span key="i" className="truncate block">{summary.proxyLogs.internal}</span>])

  return (
    <>
      <div className="fixed inset-0 z-40 bg-black/45" onClick={onClose} />
      <div className="fixed top-0 right-0 bottom-0 z-50 w-[440px] max-w-full bg-card border-l border-border flex flex-col shadow-2xl">
        <div className="flex items-center gap-2.5 px-5 py-3.5 border-b border-border">
          <SlidersHorizontal className="h-4 w-4" />
          <span className="text-sm font-semibold">Session config</span>
          {sessionKey && (
            <span className="font-mono text-xs text-muted-foreground surface-inset border border-border rounded-md px-2 py-px">
              {sessionKey}
            </span>
          )}
          <div className="ml-auto flex items-center gap-1.5">
            <CopyButton getText={() => rawJson} title="Copy config JSON" />
            <Button variant="ghost" size="icon" className="h-6 w-6" onClick={onClose} aria-label="Close config">
              <X className="h-3.5 w-3.5" />
            </Button>
          </div>
        </div>
        <div className="flex-1 overflow-y-auto p-5 flex flex-col gap-3">
          {targetRows.length > 0 && (
            <Section title="Target">
              <KeyValueGrid rows={targetRows} />
            </Section>
          )}
          {summary.envUnset && summary.envUnset.length > 0 && (
            <Section title="Environment">
              <KeyValueGrid rows={[['unset', summary.envUnset.join(', ')]]} />
            </Section>
          )}
          {summary.fsMapping && summary.fsMapping.length > 0 && (
            <Section title="File system">
              <div className="px-3.5 py-2.5 text-xs flex flex-col gap-1">
                <span className="text-muted-foreground">mapping</span>
                {summary.fsMapping.map(({ from, to }) => (
                  <div key={from} className="flex flex-col">
                    <span className="font-mono break-all">{from}</span>
                    <span className="font-mono break-all text-primary">→ {to}</span>
                  </div>
                ))}
              </div>
            </Section>
          )}
          {proxyRows.length > 0 && (
            <Section title="Proxy logs">
              <KeyValueGrid rows={proxyRows} />
            </Section>
          )}
          <button
            className={cn(
              'border border-border rounded-lg px-3.5 py-2.5 flex items-center gap-2 text-xs surface-inset',
              'hover:bg-muted/50 transition-colors text-left'
            )}
            onClick={() => setRawOpen((open) => !open)}
          >
            {rawOpen ? (
              <ChevronDown className="h-3.5 w-3.5 text-muted-foreground" />
            ) : (
              <ChevronRight className="h-3.5 w-3.5 text-muted-foreground" />
            )}
            <span className="font-semibold">Raw JSON</span>
            <span className="text-muted-foreground ml-auto tabular-nums">{rawLines} lines</span>
          </button>
          {rawOpen && (
            <div className="border border-border rounded-lg px-3.5 py-2.5 overflow-x-auto">
              <JsonHighlight value={config} />
            </div>
          )}
        </div>
      </div>
    </>
  )
}
