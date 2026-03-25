import { Card, CardContent, Badge, Button, cn } from '@metalbear/ui'
import { Cpu, Clock, Server, Trash2 } from 'lucide-react'
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
    <Card
      className={cn(
        'cursor-pointer transition-all duration-150 hover:border-primary/50',
        selected && 'border-l-4 border-l-primary border-primary/60 bg-primary/5'
      )}
      onClick={onSelect}
    >
      <CardContent className="p-3 space-y-2">
        <div className="flex items-center justify-between gap-2">
          <div className="flex items-center gap-1.5 min-w-0">
            <Server className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
            <span className="font-mono text-sm font-semibold text-foreground truncate">
              {session.target}
            </span>
          </div>
          {session.is_operator && (
            <Badge variant="secondary" className="shrink-0 text-[10px] tracking-wider">
              Operator
            </Badge>
          )}
        </div>

        <div className="flex items-center gap-3 text-xs text-muted-foreground">
          {session.processes.length > 0 && (
            <span className="flex items-center gap-1" title="Processes">
              <Cpu className="h-3 w-3" />
              {session.processes.map((p) => p.process_name || `PID ${p.pid}`).join(', ')}
            </span>
          )}
          <span className="flex items-center gap-1" title="Uptime">
            <Clock className="h-3 w-3" />
            {formatUptime(session.started_at)}
          </span>
          <span title="mirrord version" className="ml-auto font-mono">
            v{session.mirrord_version}
          </span>
        </div>

        <div className="flex items-center justify-end">
          <Button
            variant="destructive"
            size="sm"
            className="h-6 px-2 text-xs"
            onClick={(e) => {
              e.stopPropagation()
              onKill()
            }}
          >
            <Trash2 className="h-3 w-3 mr-1" />
            Kill
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}
