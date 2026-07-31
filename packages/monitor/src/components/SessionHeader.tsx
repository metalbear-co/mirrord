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
  chaosArmedCount?: number | undefined
  chaosTotalHits?: number | undefined
  onChaosClick?: (() => void) | undefined
}

export default function SessionHeader({
  session,
  processes,
  onKill,
  chaosArmedCount = 0,
  chaosTotalHits = 0,
  onChaosClick,
}: Props) {
  return (
    <div className="border-border surface-inset shrink-0 border-b px-4 py-2">
      <div className="flex items-center gap-3">
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <LiveDot active={processes.length > 0} />
          <span className="text-title text-foreground truncate font-mono">
            {session.target}
          </span>
          <Badge
            variant="outline"
            style={{ fontSize: 10 }}
            className="text-muted-foreground border-border h-4 shrink-0 px-1.5 py-0 font-medium"
          >
            {session.is_operator
              ? strings.session.operator
              : strings.session.direct}
          </Badge>
        </div>

        {chaosArmedCount > 0 && (
          <button
            type="button"
            onClick={onChaosClick}
            className="border-chaos/60 bg-chaos/10 text-chaos hover:bg-chaos/20 focus-visible:ring-ring flex shrink-0 items-center rounded-full border px-2.5 py-0.5 font-semibold transition-colors focus-visible:outline-none focus-visible:ring-1"
            style={{ fontSize: 10.5 }}
          >
            {strings.chaos.headerChip(
              chaosArmedCount,
              chaosTotalHits.toLocaleString(),
            )}
          </button>
        )}

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
