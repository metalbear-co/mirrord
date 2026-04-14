import { cn } from '@metalbear/ui'
import type { SessionInfo } from '../types'

interface StatusBarProps {
  wsConnected: boolean
  session: SessionInfo | undefined
}

export default function StatusBar({ wsConnected, session }: StatusBarProps) {
  return (
    <div className="flex items-center gap-3 px-4 py-1 border-t border-border bg-card/30 text-xs text-muted-foreground shrink-0">
      <div className="flex items-center gap-1.5">
        <div
          className={cn(
            'h-1.5 w-1.5 rounded-full',
            wsConnected ? 'bg-green-500' : 'bg-destructive'
          )}
        />
        <span>{wsConnected ? 'Connected' : 'Disconnected'}</span>
      </div>
      {session && (
        <>
          <span className="opacity-20">|</span>
          <span className="font-mono text-foreground/80">{session.target}</span>
          <span className="opacity-20">|</span>
          <span className="font-mono">{session.session_id.slice(0, 8)}</span>
          <span className="opacity-20">|</span>
          <span>{session.is_operator ? 'Operator' : 'Direct'}</span>
          {session.processes.length > 0 && (
            <>
              <span className="opacity-20">|</span>
              <span>PID {session.processes[0].pid}</span>
            </>
          )}
        </>
      )}
    </div>
  )
}
