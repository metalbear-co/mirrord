import { useState, useEffect, useRef } from 'react'
import { cn } from '@metalbear/ui'
import { Badge, Separator } from '@metalbear/ui'
import { Clock, Cpu, Server, Settings, Activity, Radio, Key } from 'lucide-react'
import type { SessionInfo, MonitorEvent } from './types'
import EventStream from './EventStream'

type DetailTab = 'overview' | 'events' | 'config'

interface Props {
  session: SessionInfo
}

interface PortSub {
  port: number
  mode: string
}

interface TrackedProcess {
  pid: number
  process_name: string
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

function OverviewTab({ session, portSubs, processes }: {
  session: SessionInfo
  portSubs: PortSub[]
  processes: TrackedProcess[]
}) {
  return (
    <div className="p-4 space-y-5 overflow-auto h-full">
      {/* Stats row */}
      <div className="grid grid-cols-3 gap-3">
        <StatCard
          icon={Cpu}
          label="Processes"
          value={processes.length}
          detail={processes.map(p => p.process_name || `PID ${p.pid}`).join(', ') || 'None connected'}
        />
        <StatCard
          icon={Clock}
          label="Uptime"
          value={formatUptime(session.started_at)}
        />
        <StatCard
          icon={Radio}
          label="Port Subscriptions"
          value={portSubs.length}
          detail={portSubs.map(p => `${p.port} (${p.mode})`).join(', ') || 'None'}
          color={portSubs.length > 0 ? '#4ade80' : undefined}
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
          {(() => {
            const rawKey = (session.config as Record<string, unknown>)?.key
            if (!rawKey) return null
            let keyStr: string
            if (typeof rawKey === 'string') {
              keyStr = rawKey
            } else if (typeof rawKey === 'object' && rawKey !== null) {
              const val = Object.values(rawKey as Record<string, unknown>)[0]
              keyStr = String(val ?? '')
            } else {
              keyStr = String(rawKey)
            }
            if (!keyStr) return null
            return (
              <div className="flex items-center justify-between px-4 py-2.5">
                <span className="text-xs text-muted-foreground">Key</span>
                <span className="text-xs font-mono text-foreground truncate max-w-[300px]">{keyStr}</span>
              </div>
            )
          })()}
        </div>
      </div>

      {/* Processes */}
      <div className="rounded-lg border border-border overflow-hidden">
        <div className="px-4 py-2 bg-card/50 border-b border-border">
          <span className="text-xs font-medium text-muted-foreground uppercase tracking-wider">Processes</span>
        </div>
        {processes.length === 0 ? (
          <div className="px-4 py-6 text-center text-xs text-muted-foreground">
            No processes connected yet
          </div>
        ) : (
          <div className="divide-y divide-border">
            {processes.map(p => (
              <div key={p.pid} className="flex items-center justify-between px-4 py-2.5">
                <div className="flex items-center gap-2">
                  <Cpu className="h-3 w-3 text-muted-foreground" />
                  <span className="text-xs font-mono font-medium text-foreground">
                    {p.process_name || 'unknown'}
                  </span>
                </div>
                <span className="text-xs font-mono text-muted-foreground">PID {p.pid}</span>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Port subscriptions */}
      {portSubs.length > 0 && (
        <div className="rounded-lg border border-border overflow-hidden">
          <div className="px-4 py-2 bg-card/50 border-b border-border">
            <span className="text-xs font-medium text-muted-foreground uppercase tracking-wider">Port Subscriptions</span>
          </div>
          <div className="divide-y divide-border">
            {portSubs.map(p => (
              <div key={p.port} className="flex items-center justify-between px-4 py-2.5">
                <div className="flex items-center gap-2">
                  <Radio className="h-3 w-3 text-muted-foreground" />
                  <span className="text-xs font-mono font-medium text-foreground">
                    :{p.port}
                  </span>
                </div>
                <Badge
                  variant="outline"
                  className={cn(
                    'text-[10px] px-2 py-0 h-5',
                    p.mode === 'steal' ? 'border-amber-500/50 text-amber-500' : 'border-green-500/50 text-green-500'
                  )}
                >
                  {p.mode}
                </Badge>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  )
}

export default function SessionDetail({ session }: Props) {
  const [activeTab, setActiveTab] = useState<DetailTab>('overview')
  const [portSubs, setPortSubs] = useState<PortSub[]>([])
  const [processes, setProcesses] = useState<TrackedProcess[]>([])
  const eventSourceRef = useRef<EventSource | null>(null)

  // Track port subscriptions and processes from the SSE event stream
  useEffect(() => {
    setPortSubs([])
    setProcesses([])

    const eventSource = new EventSource(`/api/sessions/${session.session_id}/events`)
    eventSourceRef.current = eventSource

    eventSource.onmessage = (e) => {
      let event: MonitorEvent
      try {
        event = JSON.parse(e.data)
      } catch {
        return
      }

      if (event.type === 'port_subscription') {
        setPortSubs(prev => {
          if (prev.some(p => p.port === event.port)) return prev
          return [...prev, { port: event.port, mode: event.mode }]
        })
      } else if (event.type === 'layer_connected') {
        setProcesses(prev => {
          if (prev.some(p => p.pid === event.pid)) return prev
          return [...prev, { pid: event.pid, process_name: event.process_name }]
        })
      } else if (event.type === 'layer_disconnected') {
        setProcesses(prev => prev.filter(p => p.pid !== event.pid))
      }
    }

    eventSource.onerror = () => {
      eventSource.close()
    }

    return () => {
      eventSource.close()
      eventSourceRef.current = null
    }
  }, [session.session_id])

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
        {activeTab === 'overview' && (
          <OverviewTab session={session} portSubs={portSubs} processes={processes} />
        )}
        {activeTab === 'events' && <EventStream session={session} />}
        {activeTab === 'config' && (
          <div className="p-4 overflow-auto h-full">
            <pre className="p-4 text-[11px] font-mono text-foreground/80 whitespace-pre-wrap overflow-auto rounded-lg border border-border bg-card/30">
              {JSON.stringify(session.config, null, 2)}
            </pre>
          </div>
        )}
      </div>
    </div>
  )
}
