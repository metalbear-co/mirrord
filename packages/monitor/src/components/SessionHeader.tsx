import type { ReactNode } from 'react'
import { Badge, Button } from '@metalbear/ui'
import { Trash2 } from 'lucide-react'
import type { SessionInfo, ProcessInfo } from '../types'
import { trackEvent } from '../analytics'
import { strings } from '../strings'
import LiveDot from './LiveDot'

interface Props {
  session: SessionInfo
  processes: ProcessInfo[]
  // Traffic mode (mirror/steal). Shown in the title row because it changes how the event
  // feed should be read; the rest of the session facts live in the config drawer.
  mode?: string
  trailing?: ReactNode
  onKill: () => void
}

export default function SessionHeader({ session, processes, mode, trailing, onKill }: Props) {
  return (
    <div className="border-b border-border px-4 py-2 surface-inset shrink-0">
      <div className="flex items-center gap-3">
        <div className="flex items-center gap-2 min-w-0 flex-1">
          <LiveDot active={processes.length > 0} />
          <span className="font-mono text-title text-foreground truncate">
            {session.target}
          </span>
          <Badge
            variant="outline"
            style={{ fontSize: 10 }}
            className="px-1.5 py-0 h-4 font-medium text-muted-foreground border-border shrink-0"
          >
            {session.is_operator
              ? strings.session.operator
              : strings.session.direct}
          </Badge>
          {mode && (
            <Badge
              variant="outline"
              style={{ fontSize: 10 }}
              className="px-1.5 py-0 h-4 font-mono font-medium text-foreground border-foreground/30 shrink-0"
            >
              {mode}
            </Badge>
          )}
        </div>

        {trailing}

        <Button
          variant="ghost"
          size="icon"
          onClick={() => {
            trackEvent('session_monitor_kill_session')
            onKill()
          }}
          title={strings.session.kill}
          aria-label={strings.session.kill}
          className="h-6 w-6 text-muted-foreground hover:text-destructive hover:bg-destructive/10 shrink-0"
        >
          <Trash2 className="h-3.5 w-3.5" />
        </Button>
      </div>
    </div>
  )
}
