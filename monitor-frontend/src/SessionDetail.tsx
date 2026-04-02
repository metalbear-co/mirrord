import { useState, useEffect, useRef, useCallback } from 'react'
import { cn } from '@metalbear/ui'
import { Badge, Separator } from '@metalbear/ui'
import {
  Clock, Cpu, Server, Settings, Activity, Radio, Globe, FileText,
  ArrowUpRight, ArrowDownLeft, Copy, ChevronRight, Trash2,
} from 'lucide-react'
import type { SessionInfo, MonitorEvent } from './types'
import EventStream from './EventStream'

type DetailTab = 'overview' | 'events' | 'config'

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

function extractConfigValue(obj: unknown, ...paths: string[]): string {
  let current = obj
  for (const path of paths) {
    if (current && typeof current === 'object' && path in (current as Record<string, unknown>)) {
      current = (current as Record<string, unknown>)[path]
    } else {
      return 'off'
    }
  }
  if (typeof current === 'string') return current
  if (typeof current === 'boolean') return current ? 'on' : 'off'
  if (typeof current === 'number') return String(current)
  return 'off'
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

function FeatureRow({ icon: Icon, label, value, active, count, hint }: {
  icon: typeof Activity
  label: string
  value: string
  active: boolean
  count?: number
  hint?: string
}) {
  return (
    <div className={cn(
      'flex items-center gap-3 px-3 py-2.5 rounded-md transition-colors',
      active ? 'bg-primary/5' : 'opacity-60'
    )}>
      <div className={cn(
        'flex items-center justify-center w-7 h-7 rounded-md',
        active ? 'bg-primary/10 text-primary' : 'bg-muted/50 text-muted-foreground'
      )}>
        <Icon className="h-3.5 w-3.5" />
      </div>
      <div className="flex-1 min-w-0">
        <div className="flex items-center gap-2">
          <span className="text-xs font-medium text-foreground">{label}</span>
          {count !== undefined && count > 0 && (
            <span className="text-[10px] font-mono text-primary tabular-nums">{count}</span>
          )}
        </div>
        {hint && <div className="text-[10px] text-muted-foreground truncate">{hint}</div>}
      </div>
      <Badge
        variant="outline"
        className={cn(
          'text-[9px] px-1.5 py-0 h-4 font-mono shrink-0',
          active
            ? value === 'steal' ? 'border-amber-500/40 text-amber-500 bg-amber-500/5'
              : value === 'mirror' ? 'border-green-500/40 text-green-500 bg-green-500/5'
              : 'border-primary/40 text-primary bg-primary/5'
            : 'border-border text-muted-foreground'
        )}
      >
        {value}
      </Badge>
    </div>
  )
}

function OverviewTab({ session, portSubs, processes, eventCounts, onSwitchTab }: {
  session: SessionInfo
  portSubs: PortSub[]
  processes: TrackedProcess[]
  eventCounts: EventCounts
  onSwitchTab: (tab: DetailTab) => void
}) {
  const config = session.config as Record<string, unknown>
  const incomingMode = extractConfigValue(config, 'feature', 'network', 'incoming', 'mode')
  const outgoingTcp = extractConfigValue(config, 'feature', 'network', 'outgoing', 'tcp')
  const dnsEnabled = extractConfigValue(config, 'feature', 'network', 'dns', 'enabled')
  const fsMode = extractConfigValue(config, 'feature', 'fs', 'mode')
  const envEnabled = extractConfigValue(config, 'feature', 'env', 'include') !== 'off' ||
    extractConfigValue(config, 'feature', 'env', 'exclude') !== 'off'
    ? 'on' : extractConfigValue(config, 'feature', 'env', 'unset') !== 'off' ? 'filtered' : 'off'
  const hostnameEnabled = extractConfigValue(config, 'feature', 'hostname')
  const copyTargetEnabled = extractConfigValue(config, 'feature', 'copy_target', 'enabled')

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
          <div className="flex items-center gap-2">
            <LiveDot active={processes.length > 0} />
            <span className="text-xs font-medium text-foreground">
              {processes.length > 0 ? 'Active' : 'Idle'}
            </span>
          </div>
          <Separator orientation="vertical" className="h-4" />
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

        {/* What mirrord is doing */}
        <div className="rounded-lg border border-border overflow-hidden">
          <div className="px-4 py-2.5 bg-card/50 border-b border-border flex items-center justify-between">
            <span className="text-[11px] font-semibold text-foreground uppercase tracking-wider">Active Features</span>
            <button
              onClick={() => onSwitchTab('config')}
              className="text-[10px] text-muted-foreground hover:text-foreground transition-colors flex items-center gap-1"
            >
              Full config <ChevronRight className="h-3 w-3" />
            </button>
          </div>
          <div className="p-1.5 space-y-0.5">
            <FeatureRow
              icon={ArrowDownLeft}
              label="Incoming Traffic"
              value={incomingMode}
              active={incomingMode !== 'off'}
              count={eventCounts.incoming_request}
              hint={portSubs.length > 0 ? `Ports: ${portSubs.map(p => p.port).join(', ')}` : undefined}
            />
            <FeatureRow
              icon={ArrowUpRight}
              label="Outgoing Traffic"
              value={outgoingTcp === 'on' ? 'enabled' : outgoingTcp}
              active={outgoingTcp === 'on' || outgoingTcp === 'true'}
              count={eventCounts.outgoing_connection}
            />
            <FeatureRow
              icon={Globe}
              label="DNS Resolution"
              value={dnsEnabled === 'on' ? 'enabled' : dnsEnabled}
              active={dnsEnabled === 'on' || dnsEnabled === 'true'}
              count={eventCounts.dns_query}
            />
            <FeatureRow
              icon={FileText}
              label="File Operations"
              value={fsMode}
              active={fsMode !== 'off' && fsMode !== 'disabled'}
              count={eventCounts.file_op}
              hint={fsMode === 'local' ? 'Local FS with remote fallback' : fsMode === 'read' ? 'Remote read, local write' : undefined}
            />
            <FeatureRow
              icon={Settings}
              label="Environment Variables"
              value={envEnabled}
              active={envEnabled !== 'off'}
              count={eventCounts.env_var}
            />
            <FeatureRow
              icon={Server}
              label="Hostname Override"
              value={hostnameEnabled === 'on' ? 'enabled' : hostnameEnabled}
              active={hostnameEnabled === 'on' || hostnameEnabled === 'true'}
            />
            {copyTargetEnabled !== 'off' && copyTargetEnabled !== 'false' && (
              <FeatureRow
                icon={Copy}
                label="Copy Target"
                value="enabled"
                active={true}
                hint="Running against a copy of the target pod"
              />
            )}
          </div>
        </div>

        {/* Processes */}
        {processes.length > 0 && (
          <div className="rounded-lg border border-border overflow-hidden">
            <div className="px-4 py-2.5 bg-card/50 border-b border-border">
              <span className="text-[11px] font-semibold text-foreground uppercase tracking-wider">Connected Processes</span>
            </div>
            <div className="divide-y divide-border">
              {processes.map(p => (
                <div key={p.pid} className="flex items-center gap-3 px-4 py-2.5">
                  <LiveDot active={true} />
                  <span className="text-xs font-mono font-medium text-foreground">
                    {p.process_name || 'unknown'}
                  </span>
                  <span className="text-[10px] font-mono text-muted-foreground ml-auto">PID {p.pid}</span>
                </div>
              ))}
            </div>
          </div>
        )}

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
          </div>
          <span className="text-[10px] text-muted-foreground font-mono">v{session.mirrord_version}</span>
          <button
            onClick={onKill}
            className="ml-2 text-[10px] text-destructive bg-destructive/10 border border-destructive/25 px-2.5 py-1 rounded flex items-center gap-1 hover:bg-destructive/20 transition-colors"
          >
            <Trash2 className="h-3 w-3" />
            Kill Session
          </button>
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
              {JSON.stringify(session.config, null, 2)}
            </pre>
          </div>
        )}
      </div>
    </div>
  )
}
