import { Badge, Button } from '@metalbear/ui'
import { Trash2 } from 'lucide-react'
import type { SessionInfo, ProcessInfo } from '../types'
import { trackEvent } from '../analytics'
import { strings } from '../strings'
import LiveDot from './LiveDot'

interface Props {
  session: SessionInfo
  processes: ProcessInfo[]
  onKill: () => void
}

export default function SessionHeader({ session, processes, onKill }: Props) {
  return (
    <div className="border-b border-border px-4 py-2.5 bg-card/30 shrink-0">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2.5">
          <LiveDot active={processes.length > 0} />
          <span className="font-mono text-sm font-semibold text-foreground">{session.target}</span>
          <Badge
            variant="outline"
            className={
              session.is_operator
                ? 'text-[9px] px-1.5 py-0 h-4 tracking-wider font-normal text-primary border-primary/40'
                : 'text-[9px] px-1.5 py-0 h-4 tracking-wider font-normal'
            }
          >
            {session.is_operator ? strings.session.operator : strings.session.direct}
          </Badge>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              trackEvent('session_monitor_kill_session')
              onKill()
            }}
            className="h-6 text-[10px] gap-1 px-2.5 text-destructive border-destructive/40 hover:bg-destructive/10 hover:text-destructive hover:border-destructive/60"
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
