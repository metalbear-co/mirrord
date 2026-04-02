import { useState, useEffect, useRef, useCallback } from 'react'
import { cn } from '@metalbear/ui'
import { Badge, Separator } from '@metalbear/ui'
import {
  Clock, Cpu, Server, Settings, Activity, Radio, ChevronRight, Trash2,
} from 'lucide-react'
import type { SessionInfo, MonitorEvent } from './types'
import EventStream from './EventStream'
import defaultConfig from './defaultConfig.json'
import { trackEvent } from './analytics'

type DetailTab = 'overview' | 'events' | 'config'

/** Recursively diff config against defaults, returning only values that differ. */
function diffConfig(config: unknown, defaults: unknown): unknown {
  if (config === null || config === undefined) return undefined
  if (defaults === undefined || defaults === null) return config
  if (typeof config !== typeof defaults) return config
  if (Array.isArray(config)) {
    if (!Array.isArray(defaults)) return config
    if (JSON.stringify(config) === JSON.stringify(defaults)) return undefined
    return config
  }
  if (typeof config === 'object') {
    const result: Record<string, unknown> = {}
    const cfgObj = config as Record<string, unknown>
    const defObj = defaults as Record<string, unknown>
    for (const [key, value] of Object.entries(cfgObj)) {
      const diffed = diffConfig(value, defObj[key])
      if (diffed !== undefined) result[key] = diffed
    }
    return Object.keys(result).length > 0 ? result : undefined
  }
  return config === defaults ? undefined : config
}

interface Props {
  session: SessionInfo
  onKill: () => void
}

interface PortSub {
  port: number
  mode: string
}

interface TrackedProcess {
  pid: number
  process_name: string
}

interface EventCounts {
  incoming_request: number
  outgoing_connection: number
  dns_query: number
  file_op: number
  port_subscription: number
  env_var: number
  layer_connected: number
  layer_disconnected: number
  total: number
}

function formatUptime(startedAt: string): string {
  const parsed = /^\d+$/.test(startedAt) ? Number(startedAt) * 1000 : new Date(startedAt).getTime()
  const diff = Date.now() - parsed
  const seconds = Math.floor(diff / 1000)
  const minutes = Math.floor(seconds / 60)
  const hours = Math.floor(minutes / 60)
  if (hours > 0) return `${hours}h ${minutes % 60}m`
  if (minutes > 0) return `${minutes}m ${seconds % 60}s`
  return `${seconds}s`
}


function LiveDot({ active, className }: { active: boolean; className?: string }) {
  return (
    <span className={cn('relative flex h-2 w-2', className)}>
      {active && (
        <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-green-400 opacity-75" />
      )}
      <span className={cn(
        'relative inline-flex rounded-full h-2 w-2',
        active ? 'bg-green-500' : 'bg-muted-foreground/30'
      )} />
    </span>
  )
}


function OverviewTab({ session, portSubs, processes, eventCounts, onSwitchTab }: {
  session: SessionInfo
  portSubs: PortSub[]
  processes: TrackedProcess[]
  eventCounts: EventCounts
  onSwitchTab: (tab: DetailTab) => void
}) {
  const [uptimeStr, setUptimeStr] = useState(formatUptime(session.started_at))

  useEffect(() => {
    const interval = setInterval(() => setUptimeStr(formatUptime(session.started_at)), 1000)
    return () => clearInterval(interval)
  }, [session.started_at])

  return (
    <div className="overflow-auto h-full">
      <div className="p-4 space-y-4 max-w-3xl">
        {/* Live status strip */}
        <div className="flex items-center gap-6 px-4 py-3 rounded-lg bg-card/40 border border-border">
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <Clock className="h-3 w-3" />
            <span className="font-mono tabular-nums">{uptimeStr}</span>
          </div>
          <Separator orientation="vertical" className="h-4" />
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <Cpu className="h-3 w-3" />
            <span>{processes.length} process{processes.length !== 1 ? 'es' : ''}</span>
          </div>
          <Separator orientation="vertical" className="h-4" />
          <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <Radio className="h-3 w-3" />
            <span>{portSubs.length} port{portSubs.length !== 1 ? 's' : ''}</span>
          </div>
          <Separator orientation="vertical" className="h-4" />
          <button
            onClick={() => onSwitchTab('events')}
            className="flex items-center gap-1.5 text-xs text-muted-foreground hover:text-foreground transition-colors"
          >
            <Activity className="h-3 w-3" />
            <span className="font-mono tabular-nums">{eventCounts.total}</span>
            <span>events</span>
            <ChevronRight className="h-3 w-3 opacity-50" />
          </button>
        </div>


        {/* Session identity */}
        <div className="rounded-lg border border-border overflow-hidden">
          <div className="px-4 py-2.5 bg-card/50 border-b border-border">
            <span className="text-[11px] font-semibold text-foreground uppercase tracking-wider">Session</span>
          </div>
          <div className="divide-y divide-border">
            <div className="flex items-center justify-between px-4 py-2.5">
              <span className="text-[10px] text-muted-foreground">Target</span>
              <span className="text-xs font-mono font-medium text-foreground">{session.target}</span>
            </div>
            <div className="flex items-center justify-between px-4 py-2.5">
              <span className="text-[10px] text-muted-foreground">Session ID</span>
              <span className="text-xs font-mono text-foreground">{session.session_id}</span>
            </div>
            <div className="flex items-center justify-between px-4 py-2.5">
              <span className="text-[10px] text-muted-foreground">Version</span>
              <span className="text-xs font-mono text-foreground">v{session.mirrord_version}</span>
            </div>
            {(() => {
              const rawKey = (session.config as Record<string, unknown>)?.key
              if (!rawKey) return null
              let keyStr: string
              if (typeof rawKey === 'string') keyStr = rawKey
              else if (typeof rawKey === 'object' && rawKey !== null) {
                keyStr = String(Object.values(rawKey as Record<string, unknown>)[0] ?? '')
              } else keyStr = String(rawKey)
              if (!keyStr) return null
              return (
                <div className="flex items-center justify-between px-4 py-2.5">
                  <span className="text-[10px] text-muted-foreground">Key</span>
                  <span className="text-xs font-mono text-foreground">{keyStr}</span>
                </div>
              )
            })()}
          </div>
        </div>

        {/* Port Subscriptions */}
        <div className="rounded-lg border border-border overflow-hidden">
          <div className="px-4 py-2.5 bg-card/50 border-b border-border">
            <span className="text-[11px] font-semibold text-foreground uppercase tracking-wider">Port Subscriptions</span>
          </div>
          {portSubs.length > 0 ? (
            <div className="divide-y divide-border">
              {portSubs.map(p => (
                <div key={p.port} className="flex items-center justify-between px-4 py-2.5">
                  <span className="text-xs font-mono font-medium text-foreground">:{p.port}</span>
                  <Badge variant="outline" className="text-[9px] px-1.5 py-0 h-4 font-mono">{p.mode}</Badge>
                </div>
              ))}
            </div>
          ) : (
            <div className="px-4 py-3 text-xs text-muted-foreground">No port subscriptions</div>
          )}
        </div>

        {/* Processes */}
        {processes.length > 0 && (
          <div className="rounded-lg border border-border overflow-hidden">
            <div className="px-4 py-2.5 bg-card/50 border-b border-border">
              <span className="text-[11px] font-semibold text-foreground uppercase tracking-wider">Processes</span>
            </div>
            <table className="w-full text-xs">
              <thead>
                <tr className="border-b border-border text-[10px] text-muted-foreground">
                  <th className="text-left px-4 py-2 font-medium">Name</th>
                  <th className="text-right px-4 py-2 font-medium">PID</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border">
                {processes.map(p => (
                  <tr key={p.pid}>
                    <td className="px-4 py-2.5 font-mono font-medium text-foreground">{p.process_name || 'unknown'}</td>
                    <td className="px-4 py-2.5 font-mono text-muted-foreground text-right">{p.pid}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  )
}

export default function SessionDetail({ session, onKill }: Props) {
  const [activeTab, setActiveTab] = useState<DetailTab>('overview')
  const [portSubs, setPortSubs] = useState<PortSub[]>([])
  const [processes, setProcesses] = useState<TrackedProcess[]>([])
  const [eventCounts, setEventCounts] = useState<EventCounts>({
    incoming_request: 0, outgoing_connection: 0, dns_query: 0, file_op: 0,
    port_subscription: 0, env_var: 0, layer_connected: 0, layer_disconnected: 0, total: 0,
  })

  useEffect(() => {
    setPortSubs([])
    setProcesses([])
    setEventCounts({
      incoming_request: 0, outgoing_connection: 0, dns_query: 0, file_op: 0,
      port_subscription: 0, env_var: 0, layer_connected: 0, layer_disconnected: 0, total: 0,
    })

    // Fetch fresh session info to get current processes and port subscriptions
    fetch(`/api/sessions/${session.session_id}`)
      .then(r => r.ok ? r.json() : null)
      .then((info: SessionInfo | null) => {
        if (info?.processes?.length) {
          setProcesses(info.processes.map(p => ({ pid: p.pid, process_name: p.process_name })))
        }
        if (info?.port_subscriptions?.length) {
          setPortSubs(info.port_subscriptions.map(p => ({ port: p.port, mode: p.mode })))
        }
      })
      .catch(() => {})

    const eventSource = new EventSource(`/api/sessions/${session.session_id}/events`)

    eventSource.onmessage = (e) => {
      let event: MonitorEvent
      try {
        event = JSON.parse(e.data)
      } catch {
        return
      }

      setEventCounts(prev => ({
        ...prev,
        [event.type]: (prev[event.type as keyof EventCounts] as number || 0) + 1,
        total: prev.total + 1,
      }))

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
    }
  }, [session.session_id])

  const tabs: { id: DetailTab; label: string; icon: typeof Activity; count?: number }[] = [
    { id: 'overview', label: 'Overview', icon: Server },
    { id: 'events', label: 'Events', icon: Activity, count: eventCounts.total },
    { id: 'config', label: 'Config', icon: Settings },
  ]

  return (
    <div className="h-full flex flex-col">
      {/* Session header */}
      <div className="border-b border-border px-4 py-2.5 bg-card/30 shrink-0">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2.5">
            <LiveDot active={processes.length > 0} />
            <span className="font-mono text-sm font-semibold text-foreground">{session.target}</span>
            <Badge variant="secondary" className="text-[9px] px-1.5 py-0 h-4 tracking-wider">
              {session.is_operator ? 'Operator' : 'Direct'}
            </Badge>
            <button
              onClick={() => { trackEvent('session_monitor_kill_session'); onKill() }}
              className="text-[10px] text-destructive bg-destructive/10 border border-destructive/25 px-2.5 py-1 rounded flex items-center gap-1 hover:bg-destructive/20 transition-colors"
            >
              <Trash2 className="h-3 w-3" />
              Kill
            </button>
          </div>
          <span className="text-[10px] text-muted-foreground font-mono">v{session.mirrord_version}</span>
        </div>
      </div>

      {/* Sub-tabs */}
      <div className="flex border-b border-border bg-card/20 shrink-0">
        {tabs.map((tab) => {
          const Icon = tab.icon
          return (
            <button
              key={tab.id}
              onClick={() => { trackEvent('session_monitor_tab_switch', { tab: tab.id }); setActiveTab(tab.id) }}
              className={cn(
                'flex items-center gap-1.5 px-4 py-2 text-xs font-medium border-b-2 transition-colors',
                activeTab === tab.id
                  ? 'text-foreground border-primary'
                  : 'text-muted-foreground border-transparent hover:text-foreground hover:bg-muted/30'
              )}
            >
              <Icon className="h-3 w-3" />
              {tab.label}
              {tab.count !== undefined && tab.count > 0 && (
                <span className="text-[9px] font-mono tabular-nums text-muted-foreground ml-0.5">
                  {tab.count > 999 ? `${(tab.count / 1000).toFixed(1)}k` : tab.count}
                </span>
              )}
            </button>
          )
        })}
      </div>

      {/* Tab content */}
      <div className="flex-1 overflow-hidden">
        {activeTab === 'overview' && (
          <OverviewTab
            session={session}
            portSubs={portSubs}
            processes={processes}
            eventCounts={eventCounts}
            onSwitchTab={setActiveTab}
          />
        )}
        {activeTab === 'events' && <EventStream session={session} />}
        {activeTab === 'config' && (
          <div className="p-4 overflow-auto h-full">
            <pre className="p-4 text-[11px] font-mono text-foreground/80 whitespace-pre-wrap overflow-auto rounded-lg border border-border bg-card/30">
              {JSON.stringify(diffConfig(session.config, defaultConfig), null, 2)}
            </pre>
          </div>
        )}
      </div>
    </div>
  )
}
