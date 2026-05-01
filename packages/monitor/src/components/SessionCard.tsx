import { Badge, Button } from '@metalbear/ui'
import { Trash2 } from 'lucide-react'
import type { OperatorSessionOwner, SessionInfo } from '../types'
import { strings } from '../strings'
import { formatUptime } from '../utils'
import SessionRow from './SessionRow'
import Avatar from './Avatar'

interface Props {
  session: SessionInfo
  selected: boolean
  onSelect: () => void
  onKill: () => void
  owner?: OperatorSessionOwner | null
  joined?: boolean
}

export default function SessionCard({ session, selected, onSelect, onKill, owner, joined }: Props) {
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
        style={{ fontSize: 10 }}
        className="px-1.5 py-0 h-4 font-medium text-muted-foreground border-border"
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
      right={
        owner ? (
          <Avatar
            name={owner.username}
            seed={owner.k8sUsername}
            size={20}
            ring={joined}
          />
        ) : undefined
      }
      action={
        <span className="opacity-0 group-hover:opacity-100 focus-within:opacity-100 transition-opacity">
          <Button
            variant="ghost"
            size="icon"
            onClick={(e) => {
              e.stopPropagation()
              onKill()
            }}
            title={strings.session.kill}
            aria-label={strings.session.kill}
            className="h-6 w-6 text-muted-foreground hover:text-destructive hover:bg-destructive/10"
          >
            <Trash2 className="h-3.5 w-3.5" />
          </Button>
        </span>
      }
    />
  )
}
