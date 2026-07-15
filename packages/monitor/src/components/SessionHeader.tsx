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
    <div className="border-border surface-inset shrink-0 border-b px-4 py-2">
      <div className="flex items-center gap-3">
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <LiveDot active={processes.length > 0} />
          <span className="text-title text-foreground truncate font-mono">{session.target}</span>
          <Badge
            variant="outline"
            style={{ fontSize: 10 }}
            className="text-muted-foreground border-border h-4 shrink-0 px-1.5 py-0 font-medium"
          >
            {session.is_operator ? strings.session.operator : strings.session.direct}
          </Badge>
        </div>

        <Button
          variant="ghost"
          size="icon"
          onClick={() => {
            trackEvent('session_monitor_kill_session')
            onKill()
          }}
          title={strings.session.kill}
          aria-label={strings.session.kill}
          className="text-muted-foreground hover:text-destructive hover:bg-destructive/10 h-6 w-6 shrink-0"
        >
          <Trash2 className="h-3.5 w-3.5" />
        </Button>
      </div>
    </div>
  )
}
