import { useEffect } from 'react'
import { SlidersHorizontal, X } from 'lucide-react'
import { Button } from '@metalbear/ui'
import CopyButton from './CopyButton'
import type { PortSubscription, ProcessInfo, SessionInfo } from '../types'

interface Props {
  session: SessionInfo
  portSubs: PortSubscription[]
  processes: ProcessInfo[]
  onClose: () => void
}

type Row = [string, React.ReactNode]

const isRecord = (value: unknown): value is Record<string, unknown> =>
  !!value && typeof value === 'object' && !Array.isArray(value)

// Flattens a config subtree into label/value rows with dotted paths, so every field the user
// set appears in the drawer: the sections differ only in their title, never in how much of
// the config they show. Nulls are omitted (they mean "not set" in the config diff).
function flattenRows(value: unknown, prefix = ''): Row[] {
  if (value === null || value === undefined) return []
  if (Array.isArray(value)) {
    if (value.length === 0) return []
    return [[prefix, value.map((v) => (isRecord(v) ? JSON.stringify(v) : String(v))).join(', ')]]
  }
  if (isRecord(value)) {
    return Object.entries(value).flatMap(([key, v]) =>
      flattenRows(v, prefix ? `${prefix}.${key}` : key),
    )
  }
  return [[prefix, String(value)]]
}

// `fs.mapping` keys are regex patterns, which read as noise inside a dotted path; render each
// mapping as pattern → destination instead.
function mappingRows(mapping: Record<string, unknown>): Row[] {
  return Object.entries(mapping).map(([from, to]) => [
    'mapping',
    <span key={from} className="flex flex-col">
      <span>{from}</span>
      <span className="text-primary">→ {String(to)}</span>
    </span>,
  ])
}

const SECTION_TITLES: Record<string, string> = {
  target: 'Target',
  external_proxy: 'Proxy logs',
  internal_proxy: 'Proxy logs',
  agent: 'Agent',
  operator: 'Operator',
}

const FEATURE_TITLES: Record<string, string> = {
  env: 'Environment',
  fs: 'File system',
  network: 'Network',
}

const prettify = (key: string) =>
  key.replace(/_/g, ' ').replace(/^./, (c) => c.toUpperCase())

// Turns the whole config diff into titled sections. Known keys get friendly titles and the
// `feature` subtree is split into its own sections; anything unrecognized still renders,
// generically, under its own name — nothing is editorially dropped.
function configSections(config: Record<string, unknown>): { title: string; rows: Row[] }[] {
  const sections: { title: string; rows: Row[] }[] = []
  const proxyRows: Row[] = []

  for (const [key, value] of Object.entries(config)) {
    if (value === null || value === undefined) continue
    if (key === 'key') continue
    if (key === 'external_proxy' || key === 'internal_proxy') {
      const dest = isRecord(value) ? value.log_destination : undefined
      if (typeof dest === 'string') {
        proxyRows.push([key === 'external_proxy' ? 'external' : 'internal', dest])
      } else {
        proxyRows.push(...flattenRows(value, key.replace('_proxy', '')))
      }
      continue
    }
    if (key === 'target' && isRecord(value)) {
      // target.path.{deployment,pod,...} reads better without the structural `path.` prefix:
      // the row label IS the workload kind.
      const rows = flattenRows(value).map(
        ([label, v]): Row => [label.replace(/^path\./, ''), v],
      )
      if (rows.length > 0) sections.push({ title: SECTION_TITLES.target!, rows })
      continue
    }
    if (key === 'feature' && isRecord(value)) {
      for (const [featureKey, featureValue] of Object.entries(value)) {
        if (featureKey === 'fs' && isRecord(featureValue) && isRecord(featureValue.mapping)) {
          const rest = Object.fromEntries(
            Object.entries(featureValue).filter(([k]) => k !== 'mapping'),
          )
          sections.push({
            title: FEATURE_TITLES.fs!,
            rows: [...mappingRows(featureValue.mapping), ...flattenRows(rest)],
          })
          continue
        }
        const rows = flattenRows(featureValue)
        if (rows.length > 0) {
          sections.push({ title: FEATURE_TITLES[featureKey] ?? prettify(featureKey), rows })
        }
      }
      continue
    }
    const rows = flattenRows(value)
    if (rows.length > 0) {
      sections.push({ title: SECTION_TITLES[key] ?? prettify(key), rows })
    }
  }

  if (proxyRows.length > 0) sections.push({ title: 'Proxy logs', rows: proxyRows })

  // Read-order: what you're targeting, then how traffic/env/fs behave, then diagnostics.
  const ORDER = ['Target', 'Network', 'Environment', 'File system']
  const weight = (title: string) => {
    const i = ORDER.indexOf(title)
    if (i >= 0) return i
    return title === 'Proxy logs' ? 100 : ORDER.length
  }
  return sections.sort((a, b) => weight(a.title) - weight(b.title))
}

function Section({ title, rows }: { title: string; rows: Row[] }) {
  return (
    <div className="border border-border rounded-lg overflow-hidden">
      <div className="px-3.5 py-1.5 text-[10px] font-semibold tracking-wider uppercase text-muted-foreground surface-inset border-b border-border/60">
        {title}
      </div>
      <div className="px-3.5 py-2.5 text-xs grid grid-cols-[96px_minmax(0,1fr)] gap-y-1.5 gap-x-3 items-baseline">
        {rows.map(([label, value], i) => (
          <div key={`${label}-${i}`} className="contents">
            <span className="text-muted-foreground break-words">{label}</span>
            <span className="font-mono break-all">{value}</span>
          </div>
        ))}
      </div>
    </div>
  )
}

export default function ConfigDrawer({ session, portSubs, processes, onClose }: Props) {
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

  const sessionRows: Row[] = [['session id', session.session_id]]
  if (session.key) sessionRows.push(['key', session.key])
  const started = new Date(session.started_at)
  if (!Number.isNaN(started.getTime())) {
    sessionRows.push(['started', started.toLocaleString()])
  }
  if (portSubs.length > 0) {
    sessionRows.push([
      portSubs.length === 1 ? 'port' : 'ports',
      portSubs.map((p) => `:${p.port} (${p.mode})`).join(' · '),
    ])
  }
  if (processes.length > 0) {
    sessionRows.push([
      processes.length === 1 ? 'process' : 'processes',
      processes.map((p) => `${p.process_name} ${p.pid}`).join(' · '),
    ])
  }
  sessionRows.push(['mirrord', session.mirrord_version])

  const config = session.config ?? {}

  return (
    <>
      <div className="fixed inset-0 z-40 bg-black/45" onClick={onClose} />
      <div className="fixed top-0 right-0 bottom-0 z-50 w-[440px] max-w-full bg-card border-l border-border flex flex-col shadow-2xl">
        <div className="flex items-center gap-2.5 px-5 py-3.5 border-b border-border">
          <SlidersHorizontal className="h-4 w-4" />
          <span className="text-sm font-semibold">Session config</span>
          {session.key && (
            <span className="font-mono text-xs text-muted-foreground surface-inset border border-border rounded-md px-2 py-px">
              {session.key}
            </span>
          )}
          <div className="ml-auto flex items-center gap-1.5">
            <CopyButton
              getText={() => JSON.stringify(config, null, 2)}
              title="Copy config JSON"
            />
            <Button variant="ghost" size="icon" className="h-6 w-6" onClick={onClose} aria-label="Close config">
              <X className="h-3.5 w-3.5" />
            </Button>
          </div>
        </div>
        <div className="flex-1 min-h-0 overflow-y-auto p-5 flex flex-col gap-3">
          <Section title="Session" rows={sessionRows} />
          {configSections(config).map((section, i) => (
            <Section key={`${section.title}-${i}`} title={section.title} rows={section.rows} />
          ))}
        </div>
      </div>
    </>
  )
}
