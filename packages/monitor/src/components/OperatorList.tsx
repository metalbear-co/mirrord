import { useMemo } from 'react'
import { Users, FlaskConical, Key as KeyIcon } from 'lucide-react'
import type { OperatorSessionSummary } from '../types'
import SessionRow from './SessionRow'
import Avatar from './Avatar'

interface OperatorListProps {
  sessions: OperatorSessionSummary[]
  selectedId: string | null
  onSelect: (id: string) => void
  joinedKey?: string | null
  query?: string
  emptyLabel?: string
  showCount?: boolean
}

interface KeyGroup {
  key: string
  sessions: OperatorSessionSummary[]
}

function matchesQuery(s: OperatorSessionSummary, q: string): boolean {
  if (!q) return true
  const haystack = [
    s.key,
    s.namespace,
    s.owner?.username,
    s.owner?.k8sUsername,
    s.target ? `${s.target.kind}/${s.target.name}` : '',
    s.target?.name,
    s.target?.container,
  ]
    .filter(Boolean)
    .join(' ')
    .toLowerCase()
  return haystack.includes(q)
}

function relativeTime(iso: string): string {
  const t = new Date(iso).getTime()
  if (!Number.isFinite(t)) return ''
  const diff = (Date.now() - t) / 1000
  if (diff < 60) return `${Math.max(0, Math.floor(diff))}s`
  if (diff < 3600) return `${Math.floor(diff / 60)}m`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`
  return `${Math.floor(diff / 86400)}d`
}

function firstName(full: string): string {
  return full.trim().split(/\s+/)[0] || full
}

const PREVIEW_OWNER_USERNAME = 'preview-env'

function isPreviewSession(s: { owner: { username: string } }): boolean {
  return s.owner.username === PREVIEW_OWNER_USERNAME
}

export default function OperatorList({
  sessions,
  selectedId,
  onSelect,
  joinedKey,
  query = '',
  emptyLabel = 'No teammate sessions yet.',
  showCount = false,
}: OperatorListProps) {
  const normalized = query.trim().toLowerCase()

  const filtered = useMemo(
    () => sessions.filter((s) => matchesQuery(s, normalized)),
    [sessions, normalized]
  )

  const grouped = useMemo<KeyGroup[]>(() => {
    const map = new Map<string, OperatorSessionSummary[]>()
    for (const s of filtered) {
      const arr = map.get(s.key)
      if (arr) arr.push(s)
      else map.set(s.key, [s])
    }
    const keys = Array.from(map.keys())
    keys.sort((a, b) => {
      if (a === joinedKey) return -1
      if (b === joinedKey) return 1
      return a.localeCompare(b)
    })
    return keys.map((k) => ({
      key: k,
      sessions: map.get(k)!.slice().sort((a, b) => a.id.localeCompare(b.id)),
    }))
  }, [filtered, joinedKey])

  return (
    <div className="flex flex-col gap-2.5">
      {showCount && sessions.length > 0 && (
        <div className="text-meta text-muted-foreground px-1">
          {filtered.length} session{filtered.length === 1 ? '' : 's'}
        </div>
      )}

      {grouped.length === 0 ? (
        <div className="text-center text-muted-foreground py-6">
          <Users className="h-8 w-8 mx-auto mb-2 opacity-30" />
          <p className="text-xs">
            {sessions.length === 0
              ? emptyLabel
              : 'No sessions match your search.'}
          </p>
        </div>
      ) : (
        grouped.map((g) => (
          <KeyGroupSection
            key={g.key}
            group={g}
            joined={g.key === joinedKey}
            selectedId={selectedId}
            onSelect={onSelect}
          />
        ))
      )}
    </div>
  )
}

function KeyGroupSection({
  group,
  joined,
  selectedId,
  onSelect,
}: {
  group: KeyGroup
  joined: boolean
  selectedId: string | null
  onSelect: (id: string) => void
}) {
  const groupIsPreview = group.sessions.every(isPreviewSession)
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center gap-2 px-1 text-meta font-medium text-muted-foreground">
        <KeyIcon className="h-3 w-3 shrink-0" />
        <span className="font-mono normal-case tracking-normal break-all">
          {group.key}
        </span>
        {joined && (
          <span
            style={{ fontSize: 10 }}
            className="shrink-0 px-1.5 rounded-full bg-muted text-foreground font-semibold tracking-wider"
          >
            JOINED
          </span>
        )}
        {groupIsPreview && (
          <span
            style={{ fontSize: 10 }}
            className="shrink-0 px-1.5 rounded-full border border-border text-muted-foreground font-semibold tracking-wider"
          >
            PREVIEW
          </span>
        )}
        <span className="ml-auto shrink-0 normal-case tracking-normal font-medium">
          {group.sessions.length}
        </span>
      </div>
      {group.sessions.map((s) => (
        <SessionRow
          key={s.id}
          selected={selectedId === s.id}
          onClick={() => onSelect(s.id)}
          lead={
            isPreviewSession(s) ? (
              <PreviewBadge />
            ) : (
              <Avatar name={s.owner.username} seed={s.owner.k8sUsername} />
            )
          }
          target={s.target ? `${s.target.kind}/${s.target.name}` : 'targetless'}
          meta={[
            isPreviewSession(s) ? 'preview env' : firstName(s.owner.username),
            s.namespace,
            relativeTime(s.createdAt),
          ]}
          leftStrip={joined ? 'hsl(var(--primary))' : undefined}
        />
      ))}
    </div>
  )
}

function PreviewBadge() {
  return (
    <span
      className="inline-flex items-center justify-center rounded-full bg-emerald-500/15 text-emerald-600 dark:text-emerald-300"
      style={{ width: 26, height: 26 }}
      title="preview environment"
    >
      <FlaskConical className="h-3.5 w-3.5" />
    </span>
  )
}
