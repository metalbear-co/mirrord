import { useState, useEffect } from 'react'
import { Card, CardContent, Separator } from '@metalbear/ui'
import { Clock, Cpu, Radio, Activity } from 'lucide-react'
import type { SessionInfo, PortSubscription, ProcessInfo } from '../types'
import { strings } from '../strings'
import { formatUptime } from '../utils'
import type { EventCounts } from './sessionDetailTypes'

interface Props {
  session: SessionInfo
  portSubs: PortSubscription[]
  processes: ProcessInfo[]
  eventCounts: EventCounts
}

export default function LiveStatusStrip({
  session,
  portSubs,
  processes,
  eventCounts,
}: Props) {
  const [uptimeStr, setUptimeStr] = useState(formatUptime(session.started_at))

  useEffect(() => {
    const interval = setInterval(() => setUptimeStr(formatUptime(session.started_at)), 1000)
    return () => clearInterval(interval)
  }, [session.started_at])

  const processLabel =
    processes.length !== 1 ? strings.session.processPlural : strings.session.processSingular
  const portLabel =
    portSubs.length !== 1 ? strings.session.portPlural : strings.session.portSingular

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
          <span>
            {processes.length} {processLabel}
          </span>
        </div>
        <Separator orientation="vertical" className="h-4" />
        <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <Radio className="h-3 w-3" />
          <span>
            {portSubs.length} {portLabel}
          </span>
        </div>
        <Separator orientation="vertical" className="h-4" />
        <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <Activity className="h-3 w-3" />
          <span className="font-mono tabular-nums">{eventCounts.total}</span>
          <span>{strings.session.eventsLabel}</span>
        </div>
      </CardContent>
    </Card>
  )
}
