import { useMemo } from 'react'
import { Users } from 'lucide-react'
import type { CloudSession } from '../types'
import { relativeTimeFromIso } from '../utils'
import { strings } from '../strings'
import SessionRow from './SessionRow'
import Avatar from './Avatar'

interface CloudSessionListProps {
  sessions: CloudSession[]
  query: string
}

function matchesQuery(s: CloudSession, q: string): boolean {
  if (!q) return true
  return [s.environment, s.service, s.replicaId, s.owner?.email]
    .filter(Boolean)
    .join(' ')
    .toLowerCase()
    .includes(q)
}

// Sessions of serverless targets, which the sessions-manager pairs without an operator. There's
// no detail view behind them: the sessions-manager only knows who is connected to which replica.
export default function CloudSessionList({
  sessions,
  query,
}: CloudSessionListProps) {
  const normalized = query.trim().toLowerCase()
  const filtered = useMemo(
    () =>
      sessions
        .filter((s) => matchesQuery(s, normalized))
        .sort((a, b) => b.createdAt.localeCompare(a.createdAt)),
    [sessions, normalized],
  )

  if (filtered.length === 0) {
    return (
      <div className="text-muted-foreground py-6 text-center">
        <Users className="mx-auto mb-2 h-8 w-8 opacity-30" />
        <p className="text-xs">
          {sessions.length === 0
            ? strings.sidebar.cloudEmpty
            : strings.sidebar.noSearchMatch}
        </p>
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-1">
      {filtered.map((s) => {
        const owner = s.owner?.email ?? 'unknown'
        return (
          <SessionRow
            key={s.id}
            lead={<Avatar name={owner} seed={s.owner?.userId ?? owner} />}
            target={`${s.environment}/${s.service}`}
            meta={[
              owner,
              s.replicaId,
              ...(s.connected ? [] : [strings.sidebar.cloudWaiting]),
              relativeTimeFromIso(s.createdAt),
            ]}
          />
        )
      })}
    </div>
  )
}
