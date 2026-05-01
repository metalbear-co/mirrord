import { useEffect, useState } from 'react'
import { Badge, Button } from '@metalbear/ui'
import { Activity, Clock, Cpu, Radio, Trash2 } from 'lucide-react'
import type { SessionInfo, PortSubscription, ProcessInfo } from '../types'
import { trackEvent } from '../analytics'
import { strings } from '../strings'
import { formatUptime } from '../utils'
import LiveDot from './LiveDot'
import type { EventCounts } from './sessionDetailTypes'

interface Props {
  session: SessionInfo
  processes: ProcessInfo[]
  portSubs: PortSubscription[]
  eventCounts: EventCounts
  onKill: () => void
}

export default function SessionHeader({
  session,
  processes,
  portSubs,
  eventCounts,
  onKill,
}: Props) {
  const [uptime, setUptime] = useState(formatUptime(session.started_at))
  useEffect(() => {
    const interval = setInterval(
      () => setUptime(formatUptime(session.started_at)),
      1000
    )
    return () => clearInterval(interval)
  }, [session.started_at])

  return (
    <div className="border-b border-border px-4 py-2 surface-inset shrink-0">
      <div className="flex items-center gap-x-3 gap-y-1 flex-wrap">
        <div className="flex items-center gap-2 min-w-0">
          <LiveDot active={processes.length > 0} />
          <span className="font-mono text-title text-foreground truncate">
            {session.target}
          </span>
          <Badge
            variant="outline"
            style={{ fontSize: 10 }}
            className="px-1.5 py-0 h-4 font-medium text-muted-foreground border-border"
          >
            {session.is_operator
              ? strings.session.operator
              : strings.session.direct}
          </Badge>
        </div>

        <div className="flex items-center gap-x-3 gap-y-1 text-meta text-muted-foreground flex-wrap">
          <span className="inline-flex items-center gap-1">
            <Clock className="h-3 w-3" />
            <span className="font-mono tabular-nums">{uptime}</span>
          </span>
          <span className="inline-flex items-center gap-1">
            <Cpu className="h-3 w-3" />
            {processes.length}{' '}
            {processes.length === 1
              ? strings.session.processSingular
              : strings.session.processPlural}
          </span>
          <span className="inline-flex items-center gap-1">
            <Radio className="h-3 w-3" />
            {portSubs.length}{' '}
            {portSubs.length === 1
              ? strings.session.portSingular
              : strings.session.portPlural}
          </span>
          <span className="inline-flex items-center gap-1">
            <Activity className="h-3 w-3" />
            <span className="font-mono tabular-nums">{eventCounts.total}</span>
            {strings.session.eventsLabel}
          </span>
        </div>

        <div className="ml-auto flex items-center gap-3">
          <span className="text-meta text-muted-foreground font-mono">
            v{session.mirrord_version}
          </span>
          <Button
            variant="ghost"
            size="icon"
            onClick={() => {
              trackEvent('session_monitor_kill_session')
              onKill()
            }}
            title={strings.session.kill}
            aria-label={strings.session.kill}
            className="h-6 w-6 text-muted-foreground hover:text-destructive hover:bg-destructive/10"
          >
            <Trash2 className="h-3.5 w-3.5" />
          </Button>
        </div>
      </div>
    </div>
  )
}
