import { useState } from 'react'
import { cn } from '@metalbear/ui'
import { Badge, Separator } from '@metalbear/ui'
import { Clock, Cpu, Server, Settings, Activity, FileText, Globe, Zap, Shield } from 'lucide-react'
import type { SessionInfo } from './types'
import EventStream from './EventStream'

type DetailTab = 'overview' | 'events' | 'config'

interface Props {
  session: SessionInfo
}

function formatUptime(startedAt: string): string {
  const parsed = /^\d+$/.test(startedAt) ? Number(startedAt) * 1000 : new Date(startedAt).getTime()
  const diff = Date.now() - parsed
  const seconds = Math.floor(diff / 1000)
  const minutes = Math.floor(seconds / 60)
  const hours = Math.floor(minutes / 60)
  if (hours > 0) return `${hours}h ${minutes % 60}m ${seconds % 60}s`
  if (minutes > 0) return `${minutes}m ${seconds % 60}s`
  return `${seconds}s`
}

function StatCard({ icon: Icon, label, value, detail, color }: {
  icon: typeof Activity
  label: string
  value: string | number
  detail?: string
  color?: string
}) {
  return (
    <div className="rounded-lg border border-border bg-card/30 p-3">
      <div className="flex items-center gap-2 mb-1.5">
        <Icon className="h-3.5 w-3.5 text-muted-foreground" style={color ? { color } : undefined} />
        <span className="text-[10px] text-muted-foreground uppercase tracking-wider font-medium">{label}</span>
      </div>
      <div className="text-lg font-bold text-foreground">{value}</div>
      {detail && <div className="text-[10px] text-muted-foreground mt-0.5">{detail}</div>}
    </div>
  )
}

function ConfigSection({ config }: { config: Record<string, unknown> }) {
  const items: { label: string; value: string }[] = []

  const targetPath = config.target as Record<string, unknown> | undefined
  if (targetPath?.path) {
    const path = targetPath.path as Record<string, unknown>
    const kind = Object.keys(path).find(k => k !== 'container') ?? 'target'
    items.push({ label: 'Target', value: `${kind}/${path[kind] ?? ''}` })
  }
  if (targetPath?.namespace) items.push({ label: 'Namespace', value: String(targetPath.namespace) })

  items.push({ label: 'Incoming', value: extractConfigValue(config, 'feature', 'network', 'incoming', 'mode') })
  items.push({ label: 'Outgoing TCP', value: extractConfigValue(config, 'feature', 'network', 'outgoing', 'tcp') })
  items.push({ label: 'DNS', value: extractConfigValue(config, 'feature', 'network', 'dns', 'enabled') })
  items.push({ label: 'File System', value: extractConfigValue(config, 'feature', 'fs', 'mode') })
  items.push({ label: 'Hostname', value: extractConfigValue(config, 'feature', 'hostname') })

  return (
    <div className="space-y-4">
      <div className="rounded-lg border border-border overflow-hidden">
        <div className="px-4 py-2 bg-card/50 border-b border-border">
          <span className="text-xs font-medium text-muted-foreground uppercase tracking-wider">Configuration Summary</span>
        </div>
        <div className="divide-y divide-border">
          {items.map(({ label, value }) => (
            <div key={label} className="flex items-center justify-between px-4 py-2.5">
              <span className="text-xs text-muted-foreground">{label}</span>
              <span className="text-xs font-mono font-medium text-foreground">{value}</span>
            </div>
          ))}
        </div>
      </div>

      <div className="rounded-lg border border-border overflow-hidden">
        <div className="px-4 py-2 bg-card/50 border-b border-border">
          <span className="text-xs font-medium text-muted-foreground uppercase tracking-wider">Raw Config</span>
        </div>
        <pre className="p-4 text-[11px] font-mono text-foreground/80 whitespace-pre-wrap overflow-auto max-h-[400px]">
          {JSON.stringify(config, null, 2)}
        </pre>
      </div>
    </div>
  )
}

function extractConfigValue(obj: unknown, ...paths: string[]): string {
  let current = obj
  for (const path of paths) {
    if (current && typeof current === 'object' && path in (current as Record<string, unknown>)) {
      current = (current as Record<string, unknown>)[path]
    } else {
      return 'disabled'
    }
  }
  if (typeof current === 'string') return current
  if (typeof current === 'boolean') return current ? 'enabled' : 'disabled'
  if (typeof current === 'number') return String(current)
  return 'disabled'
}

function OverviewTab({ session }: { session: SessionInfo }) {
  const config = session.config as Record<string, unknown>
  const incomingMode = extractConfigValue(config, 'feature', 'network', 'incoming', 'mode')
  const fsMode = extractConfigValue(config, 'feature', 'fs', 'mode')
  const dnsEnabled = extractConfigValue(config, 'feature', 'network', 'dns', 'enabled')

  return (
    <div className="p-4 space-y-5 overflow-auto h-full">
      {/* Stats row */}
      <div className="grid grid-cols-4 gap-3">
        <StatCard
          icon={Cpu}
          label="Processes"
          value={session.processes.length}
          detail={session.processes.map(p => p.process_name || `PID ${p.pid}`).join(', ') || 'None connected'}
        />
        <StatCard
          icon={Clock}
          label="Uptime"
          value={formatUptime(session.started_at)}
        />
        <StatCard
          icon={Zap}
          label="Incoming"
          value={incomingMode}
          color={incomingMode === 'steal' ? '#f59e0b' : incomingMode === 'mirror' ? '#4ade80' : undefined}
        />
        <StatCard
          icon={FileText}
          label="File System"
          value={fsMode}
        />
      </div>

      {/* Session info */}
      <div className="rounded-lg border border-border overflow-hidden">
        <div className="px-4 py-2 bg-card/50 border-b border-border">
          <span className="text-xs font-medium text-muted-foreground uppercase tracking-wider">Session Details</span>
        </div>
        <div className="divide-y divide-border">
          <div className="flex items-center justify-between px-4 py-2.5">
            <span className="text-xs text-muted-foreground">Session ID</span>
            <span className="text-xs font-mono text-foreground">{session.session_id}</span>
          </div>
          <div className="flex items-center justify-between px-4 py-2.5">
            <span className="text-xs text-muted-foreground">Target</span>
            <span className="text-xs font-mono font-medium text-foreground">{session.target}</span>
          </div>
          <div className="flex items-center justify-between px-4 py-2.5">
            <span className="text-xs text-muted-foreground">mirrord Version</span>
            <span className="text-xs font-mono text-foreground">v{session.mirrord_version}</span>
          </div>
          <div className="flex items-center justify-between px-4 py-2.5">
            <span className="text-xs text-muted-foreground">Mode</span>
            <Badge variant="secondary" className="text-[10px] px-2 py-0 h-5">
              {session.is_operator ? 'Operator' : 'Direct'}
            </Badge>
          </div>
          {session.processes.length > 0 && (
            <div className="flex items-center justify-between px-4 py-2.5">
              <span className="text-xs text-muted-foreground">Processes</span>
              <div className="flex gap-1.5">
                {session.processes.map(p => (
                  <Badge key={p.pid} variant="outline" className="text-[10px] px-2 py-0 h-5 font-mono">
                    {p.process_name || 'unknown'} ({p.pid})
                  </Badge>
                ))}
              </div>
            </div>
          )}
        </div>
      </div>

      {/* Config summary */}
      <div className="rounded-lg border border-border overflow-hidden">
        <div className="px-4 py-2 bg-card/50 border-b border-border">
          <span className="text-xs font-medium text-muted-foreground uppercase tracking-wider">Feature Configuration</span>
        </div>
        <div className="grid grid-cols-3 divide-x divide-border">
          <div className="p-3 text-center">
            <Globe className="h-4 w-4 mx-auto mb-1 text-muted-foreground" />
            <div className="text-[10px] text-muted-foreground mb-0.5">Network</div>
            <div className="text-xs font-medium text-foreground">{incomingMode}</div>
          </div>
          <div className="p-3 text-center">
            <FileText className="h-4 w-4 mx-auto mb-1 text-muted-foreground" />
            <div className="text-[10px] text-muted-foreground mb-0.5">File System</div>
            <div className="text-xs font-medium text-foreground">{fsMode}</div>
          </div>
          <div className="p-3 text-center">
            <Shield className="h-4 w-4 mx-auto mb-1 text-muted-foreground" />
            <div className="text-[10px] text-muted-foreground mb-0.5">DNS</div>
            <div className="text-xs font-medium text-foreground">{dnsEnabled}</div>
          </div>
        </div>
      </div>
    </div>
  )
}

export default function SessionDetail({ session }: Props) {
  const [activeTab, setActiveTab] = useState<DetailTab>('overview')

  const tabs: { id: DetailTab; label: string; icon: typeof Activity }[] = [
    { id: 'overview', label: 'Overview', icon: Server },
    { id: 'events', label: 'Events', icon: Activity },
    { id: 'config', label: 'Config', icon: Settings },
  ]

  return (
    <div className="h-full flex flex-col">
      {/* Session header */}
      <div className="border-b border-border px-4 py-3 bg-card/30 shrink-0">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2.5">
            <Server className="h-4 w-4 text-muted-foreground" />
            <span className="font-mono text-sm font-semibold text-foreground">{session.target}</span>
            <Separator orientation="vertical" className="h-4" />
            <span className="text-xs text-muted-foreground font-mono">v{session.mirrord_version}</span>
            <Badge variant="secondary" className="text-[9px] px-1.5 py-0 h-4 tracking-wider">
              {session.is_operator ? 'Operator' : 'Direct'}
            </Badge>
          </div>
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <Clock className="h-3 w-3" />
            <span>{formatUptime(session.started_at)}</span>
          </div>
        </div>
      </div>

      {/* Sub-tabs */}
      <div className="flex border-b border-border bg-card/20 shrink-0">
        {tabs.map((tab) => {
          const Icon = tab.icon
          return (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={cn(
                'flex items-center gap-1.5 px-4 py-2 text-xs font-medium border-b-2 transition-colors',
                activeTab === tab.id
                  ? 'text-foreground border-primary'
                  : 'text-muted-foreground border-transparent hover:text-foreground hover:bg-muted/30'
              )}
            >
              <Icon className="h-3 w-3" />
              {tab.label}
            </button>
          )
        })}
      </div>

      {/* Tab content */}
      <div className="flex-1 overflow-hidden">
        {activeTab === 'overview' && <OverviewTab session={session} />}
        {activeTab === 'events' && <EventStream session={session} />}
        {activeTab === 'config' && (
          <div className="p-4 overflow-auto h-full">
            <ConfigSection config={session.config as Record<string, unknown>} />
          </div>
        )}
      </div>
    </div>
  )
}
