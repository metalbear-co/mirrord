import type { ReactNode } from 'react'
import { Badge } from '@metalbear/ui'
import type { SessionInfo, ProcessInfo } from '../types'
import { strings } from '../strings'
import LiveDot from './LiveDot'

interface Props {
  session: SessionInfo
  processes: ProcessInfo[]
  // Traffic mode (mirror/steal). Shown in the title row because it changes how the event
  // feed should be read; the rest of the session facts live in the config drawer.
  mode?: string
  // Right-side actions (extension CTA, session actions menu), owned by the caller.
  trailing?: ReactNode
}

export default function SessionHeader({ session, processes, mode, trailing }: Props) {
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
      </div>
    </div>
  )
}
