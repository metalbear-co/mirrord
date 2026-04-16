import { Badge, Button, Card, CardContent, cn } from '@metalbear/ui'
import { Clock, Trash2 } from 'lucide-react'
import type { SessionInfo } from '../types'
import { strings } from '../strings'
import { formatUptime } from '../utils'

interface Props {
  session: SessionInfo
  selected: boolean
  onSelect: () => void
  onKill: () => void
}

export default function SessionCard({ session, selected, onSelect, onKill }: Props) {
  return (
    <Card
      onClick={onSelect}
      className={cn(
        'cursor-pointer transition-all duration-150 border group',
        selected
          ? 'border-border bg-muted/40'
          : 'border-transparent hover:bg-muted/40 hover:border-border'
      )}
    >
      <CardContent className="px-3 py-2.5 space-y-1.5">
        <div className="font-mono text-xs font-medium text-foreground break-all leading-snug">
          {session.target}
        </div>

        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2 text-[10px] text-muted-foreground">
            <span className="flex items-center gap-1">
              <Clock className="h-2.5 w-2.5" />
              {formatUptime(session.started_at)}
            </span>
            <span className="font-mono">v{session.mirrord_version}</span>
            {session.is_operator && (
              <Badge variant="secondary" className="text-[9px] px-1.5 py-0 h-4 tracking-wider">
                {strings.session.operator}
              </Badge>
            )}
          </div>
          <Button
            variant="destructive"
            size="sm"
            onClick={(e) => {
              e.stopPropagation()
              onKill()
            }}
            className="h-5 text-[9px] gap-1 px-2"
          >
            <Trash2 className="h-2.5 w-2.5" />
            {strings.session.kill}
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}
