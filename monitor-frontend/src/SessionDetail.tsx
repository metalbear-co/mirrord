import { useState, useEffect } from 'react'
import {
  Badge, Button, Card, CardContent, CardHeader, Separator, Table, TableBody,
  TableCell, TableHead, TableHeader, TableRow, cn,
} from '@metalbear/ui'
import {
  Clock, Cpu, Server, Settings, Activity, Radio, ChevronRight, Trash2,
} from 'lucide-react'
import type { SessionInfo, MonitorEvent } from './types'
import EventStream from './EventStream'
import { trackEvent } from './analytics'
import { api } from './api'
import { strings } from './strings'
import { EventType } from './eventTypes'

type DetailTab = 'overview' | 'events' | 'config'

// Session config may carry the license `key` as either a plain string or an
// object shape (e.g. { value: "..." }); flatten it to a displayable string.
function extractLicenseKey(config: unknown): string | null {
  const rawKey = (config as Record<string, unknown> | null)?.key
  if (!rawKey) return null
  if (typeof rawKey === 'string') return rawKey
  if (typeof rawKey === 'object') {
    const firstValue = Object.values(rawKey as Record<string, unknown>)[0]
    return firstValue ? String(firstValue) : null
  }
  return String(rawKey)
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
        <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-primary opacity-75" />
      )}
      <span className={cn(
        'relative inline-flex rounded-full h-2 w-2',
        active ? 'bg-primary' : 'bg-muted-foreground/30'
      )} />
    </span>
  )
}


function LiveStatusStrip({
  session, portSubs, processes, eventCounts, onSwitchTab,
}: {
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

  const processLabel = processes.length !== 1 ? strings.session.processPlural : strings.session.processSingular
  const portLabel = portSubs.length !== 1 ? strings.session.portPlural : strings.session.portSingular

  return (
    <Card className="bg-card/40">
      <CardContent className="flex items-center gap-6 px-4 py-3">
        <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <Clock className="h-3 w-3" />
          <span className="font-mono tabular-nums">{uptimeStr}</span>
        </div>
        <Separator orientation="vertical" className="h-4" />
        <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <Cpu className="h-3 w-3" />
          <span>{processes.length} {processLabel}</span>
        </div>
        <Separator orientation="vertical" className="h-4" />
        <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <Radio className="h-3 w-3" />
          <span>{portSubs.length} {portLabel}</span>
        </div>
        <Separator orientation="vertical" className="h-4" />
        <Button
          variant="ghost"
          size="sm"
          onClick={() => onSwitchTab('events')}
          className="h-auto px-1.5 py-0.5 text-xs text-muted-foreground hover:text-foreground gap-1.5"
        >
          <Activity className="h-3 w-3" />
          <span className="font-mono tabular-nums">{eventCounts.total}</span>
          <span>{strings.session.eventsLabel}</span>
          <ChevronRight className="h-3 w-3 opacity-50" />
        </Button>
      </CardContent>
    </Card>
  )
}

function SessionIdentity({ session }: { session: SessionInfo }) {
  const licenseKey = extractLicenseKey(session.config)

  return (
    <Card className="overflow-hidden p-0">
      <CardHeader className="px-4 py-2.5 bg-card/50 border-b border-border">
        <span className="text-[11px] font-semibold text-foreground uppercase tracking-wider">
          {strings.session.sectionSession}
        </span>
      </CardHeader>
      <CardContent className="p-0 divide-y divide-border">
        <div className="flex items-center justify-between px-4 py-2.5">
          <span className="text-[10px] text-muted-foreground">{strings.session.fieldTarget}</span>
          <span className="text-xs font-mono font-medium text-foreground">{session.target}</span>
        </div>
        <div className="flex items-center justify-between px-4 py-2.5">
          <span className="text-[10px] text-muted-foreground">{strings.session.fieldSessionId}</span>
          <span className="text-xs font-mono text-foreground">{session.session_id}</span>
        </div>
        <div className="flex items-center justify-between px-4 py-2.5">
          <span className="text-[10px] text-muted-foreground">{strings.session.fieldVersion}</span>
          <span className="text-xs font-mono text-foreground">v{session.mirrord_version}</span>
        </div>
        {licenseKey && (
          <div className="flex items-center justify-between px-4 py-2.5">
            <span className="text-[10px] text-muted-foreground">{strings.session.fieldKey}</span>
            <span className="text-xs font-mono text-foreground">{licenseKey}</span>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

function PortSubscriptionsCard({ portSubs }: { portSubs: PortSub[] }) {
  return (
    <Card className="overflow-hidden p-0">
      <CardHeader className="px-4 py-2.5 bg-card/50 border-b border-border">
        <span className="text-[11px] font-semibold text-foreground uppercase tracking-wider">
          {strings.session.sectionPorts}
        </span>
      </CardHeader>
      <CardContent className="p-0">
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
          <div className="px-4 py-3 text-xs text-muted-foreground">{strings.session.noPorts}</div>
        )}
      </CardContent>
    </Card>
  )
}

function ProcessesCard({ processes }: { processes: TrackedProcess[] }) {
  if (processes.length === 0) return null
  return (
    <Card className="overflow-hidden p-0">
      <CardHeader className="px-4 py-2.5 bg-card/50 border-b border-border">
        <span className="text-[11px] font-semibold text-foreground uppercase tracking-wider">
          {strings.session.sectionProcesses}
        </span>
      </CardHeader>
      <CardContent className="p-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{strings.session.columnName}</TableHead>
              <TableHead className="text-right">{strings.session.columnPid}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {processes.map(p => (
              <TableRow key={p.pid}>
                <TableCell className="font-mono font-medium text-foreground">
                  {p.process_name || strings.session.unknownProcess}
                </TableCell>
                <TableCell className="font-mono text-muted-foreground text-right">{p.pid}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  )
}

function OverviewTab({ session, portSubs, processes, eventCounts, onSwitchTab }: {
  session: SessionInfo
  portSubs: PortSub[]
  processes: TrackedProcess[]
  eventCounts: EventCounts
  onSwitchTab: (tab: DetailTab) => void
}) {
  return (
    <div className="overflow-auto h-full">
      <div className="p-4 space-y-4 max-w-3xl">
        <LiveStatusStrip
          session={session}
          portSubs={portSubs}
          processes={processes}
          eventCounts={eventCounts}
          onSwitchTab={onSwitchTab}
        />
        <SessionIdentity session={session} />
        <PortSubscriptionsCard portSubs={portSubs} />
        <ProcessesCard processes={processes} />
      </div>
    </div>
  )
}

function ConfigTab({ config }: { config: unknown }) {
  return (
    <div className="p-4 overflow-auto h-full">
      <pre className="p-4 text-[11px] font-mono text-foreground/80 whitespace-pre-wrap overflow-auto rounded-lg border border-border bg-card/30">
        {JSON.stringify(config, null, 2)}
      </pre>
    </div>
  )
}

function SessionHeader({
  session, processes, onKill,
}: {
  session: SessionInfo
  processes: TrackedProcess[]
  onKill: () => void
}) {
  return (
    <div className="border-b border-border px-4 py-2.5 bg-card/30 shrink-0">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2.5">
          <LiveDot active={processes.length > 0} />
          <span className="font-mono text-sm font-semibold text-foreground">{session.target}</span>
          <Badge variant="secondary" className="text-[9px] px-1.5 py-0 h-4 tracking-wider">
            {session.is_operator ? strings.session.operator : strings.session.direct}
          </Badge>
          <Button
            variant="destructive"
            size="sm"
            onClick={() => { trackEvent('session_monitor_kill_session'); onKill() }}
            className="h-6 text-[10px] gap-1 px-2.5"
          >
            <Trash2 className="h-3 w-3" />
            {strings.session.kill}
          </Button>
        </div>
        <span className="text-[10px] text-muted-foreground font-mono">v{session.mirrord_version}</span>
      </div>
    </div>
  )
}

interface TabDef {
  id: DetailTab
  label: string
  icon: typeof Activity
  count?: number
}

function SessionTabs({
  tabs, activeTab, onTabChange,
}: {
  tabs: TabDef[]
  activeTab: DetailTab
  onTabChange: (tab: DetailTab) => void
}) {
  return (
    <div className="flex border-b border-border bg-card/20 shrink-0">
      {tabs.map((tab) => {
        const Icon = tab.icon
        return (
          <button
            key={tab.id}
            onClick={() => { trackEvent('session_monitor_tab_switch', { tab: tab.id }); onTabChange(tab.id) }}
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
    api.getSession(session.session_id)
      .then((info) => {
        if (info?.processes?.length) {
          setProcesses(info.processes.map(p => ({ pid: p.pid, process_name: p.process_name })))
        }
        if (info?.port_subscriptions?.length) {
          setPortSubs(info.port_subscriptions.map(p => ({ port: p.port, mode: p.mode })))
        }
      })
      .catch(() => {})

    const eventSource = new EventSource(api.eventStreamUrl(session.session_id))

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

      if (event.type === EventType.PortSubscription) {
        setPortSubs(prev => {
          if (prev.some(p => p.port === event.port)) return prev
          return [...prev, { port: event.port, mode: event.mode }]
        })
      } else if (event.type === EventType.LayerConnected) {
        setProcesses(prev => {
          if (prev.some(p => p.pid === event.pid)) return prev
          return [...prev, { pid: event.pid, process_name: event.process_name }]
        })
      } else if (event.type === EventType.LayerDisconnected) {
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

  const tabs: TabDef[] = [
    { id: 'overview', label: strings.session.overview, icon: Server },
    { id: 'events', label: strings.session.eventsTab, icon: Activity, count: eventCounts.total },
    { id: 'config', label: strings.session.configTab, icon: Settings },
  ]

  return (
    <div className="h-full flex flex-col">
      <SessionHeader session={session} processes={processes} onKill={onKill} />
      <SessionTabs tabs={tabs} activeTab={activeTab} onTabChange={setActiveTab} />
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
        {activeTab === 'config' && <ConfigTab config={session.config} />}
      </div>
    </div>
  )
}
