import { Badge, Button } from '@metalbear/ui'
import { Trash2 } from 'lucide-react'
import type { SessionInfo } from '../types'
import { strings } from '../strings'
import { formatUptime } from '../utils'
import SessionRow from './SessionRow'

interface Props {
  session: SessionInfo
  selected: boolean
  onSelect: () => void
  onKill: () => void
}

export default function SessionCard({ session, selected, onSelect, onKill }: Props) {
  const meta: (string | React.ReactNode)[] = [
    formatUptime(session.started_at),
    `${session.processes.length} proc`,
    `v${session.mirrord_version}`,
  ]
  if (session.is_operator) {
    meta.push(
      <Badge
        key="op"
        variant="outline"
        className="text-caps px-1.5 py-0 h-4 tracking-wider font-medium text-primary border-primary/40"
      >
        {strings.session.operator}
      </Badge>
    )
  }

  return (
    <SessionRow
      selected={selected}
      onClick={onSelect}
      lead={
        <span className="inline-flex items-center justify-center">
          <span className="w-2.5 h-2.5 rounded-full bg-emerald-500" />
        </span>
      }
      target={session.target}
      meta={meta}
      action={
        <span className="opacity-0 group-hover:opacity-100 focus-within:opacity-100 transition-opacity">
          <Button
            variant="outline"
            size="sm"
            onClick={(e) => {
              e.stopPropagation()
              onKill()
            }}
            className="h-6 text-caps gap-1 px-2 text-destructive border-destructive/40 hover:bg-destructive/10 hover:text-destructive hover:border-destructive/60"
          >
            <Trash2 className="h-3 w-3" />
            {strings.session.kill}
          </Button>
        </span>
      }
    />
  )
}
