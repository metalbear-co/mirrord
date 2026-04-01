import { cn } from '@metalbear/ui'
import { Badge } from '@metalbear/ui'
import { Clock, Trash2 } from 'lucide-react'
import type { SessionInfo } from './types'

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

interface Props {
  session: SessionInfo
  selected: boolean
  onSelect: () => void
  onKill: () => void
}

export default function SessionCard({ session, selected, onSelect, onKill }: Props) {
  return (
    <div
      className={cn(
        'rounded-md px-3 py-2.5 cursor-pointer transition-all duration-150 border border-transparent group',
        selected
          ? 'bg-primary/8 border-primary/30 border-l-[3px] border-l-primary'
          : 'hover:bg-muted/40 hover:border-border'
      )}
      onClick={onSelect}
    >
      <div className="font-mono text-xs font-medium text-foreground break-all leading-snug">
        {session.target}
      </div>

      <div className="flex items-center justify-between mt-1.5">
        <div className="flex items-center gap-2 text-[10px] text-muted-foreground">
          <span className="flex items-center gap-1">
            <Clock className="h-2.5 w-2.5" />
            {formatUptime(session.started_at)}
          </span>
          <span className="font-mono">v{session.mirrord_version}</span>
          {session.is_operator && (
            <Badge variant="secondary" className="text-[9px] px-1.5 py-0 h-4 tracking-wider">
              Operator
            </Badge>
          )}
        </div>
        <button
          className="text-[9px] text-destructive bg-destructive/10 border border-destructive/25 px-2 py-0.5 rounded cursor-pointer opacity-0 group-hover:opacity-100 transition-opacity"
          onClick={(e) => {
            e.stopPropagation()
            onKill()
          }}
        >
          <Trash2 className="h-2.5 w-2.5 inline mr-0.5" />
          Kill
        </button>
      </div>
    </div>
  )
}
