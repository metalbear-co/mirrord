import { Button } from '@metalbear/ui'
import { Settings, Trash2 } from 'lucide-react'
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
  onConfig: () => void
  owner?: OperatorSessionOwner | null
  joined?: boolean
  currentContext: string | null
  currentNamespace: string | null
}

// A gke context is `gke_<project>_<zone>_<cluster>`; the cluster (last segment) is the part
// that distinguishes it, so show that rather than truncating away to the project prefix.
function shortContext(context: string): string {
  if (context.startsWith('gke_')) {
    const parts = context.split('_')
    return parts[parts.length - 1] || context
  }
  return context
}

export default function SessionCard({
  session,
  selected,
  onSelect,
  onKill,
  onConfig,
  owner,
  joined,
  currentContext,
  currentNamespace,
}: Props) {
  const meta: (string | React.ReactNode)[] = [formatUptime(session.started_at)]
  // Local sessions are shown regardless of the selected context/namespace, so label the ones that
  // differ from the top-bar selection; a location matching what's already selected is noise.
  const contextDiffers = !!session.context && session.context !== currentContext
  const namespaceDiffers = !!session.namespace && session.namespace !== currentNamespace
  const fullLocation = [session.context, session.namespace].filter(Boolean).join(' / ')
  const shortLocation = [
    contextDiffers && session.context ? shortContext(session.context) : null,
    namespaceDiffers ? session.namespace : null,
  ]
    .filter(Boolean)
    .join(' / ')
  if (shortLocation) {
    meta.push(
      <span
        key="loc"
        title={fullLocation}
        className="font-mono text-muted-foreground max-w-[180px] truncate"
      >
        {shortLocation}
      </span>
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
        <span className="opacity-0 group-hover:opacity-100 focus-within:opacity-100 transition-opacity inline-flex items-center gap-0.5">
          <Button
            variant="ghost"
            size="icon"
            onClick={(e) => {
              e.stopPropagation()
              onConfig()
            }}
            title={strings.session.config}
            aria-label={strings.session.config}
            className="h-6 w-6 text-muted-foreground hover:text-foreground"
          >
            <Settings className="h-3.5 w-3.5" />
          </Button>
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
