import { Badge, Button } from '@metalbear/ui'
import { Server, Trash2 } from 'lucide-react'
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
  const meta: (string | React.ReactNode)[] = [formatUptime(session.started_at)]
  // Local sessions are shown regardless of the selected context/namespace, so label each with its
  // own so it's clear which cluster it belongs to.
  const location = [session.context, session.namespace].filter(Boolean).join(' / ')
  if (location) {
    meta.push(
      <span
        key="loc"
        title={location}
        className="text-muted-foreground inline-flex max-w-[180px] items-center gap-1 truncate font-mono"
      >
        <Server className="h-3 w-3 shrink-0 opacity-70" />
        {location}
      </span>,
    )
  }
  if (session.is_operator) {
    meta.push(
      <Badge
        key="op"
        variant="outline"
        style={{ fontSize: 10 }}
        className="text-muted-foreground border-border h-4 px-1.5 py-0 font-medium"
      >
        {strings.session.operator}
      </Badge>,
    )
  }

  return (
    <SessionRow
      selected={selected}
      onClick={onSelect}
      lead={
        <span className="inline-flex items-center justify-center">
          <span className="h-2.5 w-2.5 rounded-full bg-emerald-500" />
        </span>
      }
      target={session.target}
      meta={meta}
      right={
        owner ? (
          <Avatar name={owner.username} seed={owner.k8sUsername} size={20} ring={joined} />
        ) : undefined
      }
      action={
        <span className="opacity-0 transition-opacity focus-within:opacity-100 group-hover:opacity-100">
          <Button
            variant="ghost"
            size="icon"
            onClick={(e) => {
              e.stopPropagation()
              onKill()
            }}
            title={strings.session.kill}
            aria-label={strings.session.kill}
            className="text-muted-foreground hover:text-destructive hover:bg-destructive/10 h-6 w-6"
          >
            <Trash2 className="h-3.5 w-3.5" />
          </Button>
        </span>
      }
    />
  )
}
