import { useMemo } from 'react'
import { Users, FlaskConical, Key as KeyIcon } from 'lucide-react'
import type { OperatorSessionSummary } from '../types'
import {
  firstName,
  isPreviewSession,
  previewPhaseLabel,
  previewPhaseTone,
  relativeTimeFromIso,
  type PreviewTone,
} from '../utils'
import { strings } from '../strings'
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
    s.owner.username,
    s.owner.k8sUsername,
    s.target ? `${s.target.kind}/${s.target.name}` : '',
    s.target?.name,
    s.target?.container,
  ]
    .filter(Boolean)
    .join(' ')
    .toLowerCase()
  return haystack.includes(q)
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
    [sessions, normalized],
  )

  const grouped = useMemo<KeyGroup[]>(() => {
    const map = new Map<string, OperatorSessionSummary[]>()
    for (const s of filtered) {
      const arr = map.get(s.key)
      if (arr) arr.push(s)
      else map.set(s.key, [s])
    }
    const entries = Array.from(map.entries())
    entries.sort(([a], [b]) => {
      if (a === joinedKey) return -1
      if (b === joinedKey) return 1
      return a.localeCompare(b)
    })
    return entries.map(([k, group]) => ({
      key: k,
      sessions: group.slice().sort((a, b) => a.id.localeCompare(b.id)),
    }))
  }, [filtered, joinedKey])

  return (
    <div className="flex flex-col gap-2.5">
      {showCount && sessions.length > 0 && (
        <div className="text-meta text-muted-foreground px-1">
          {filtered.length}{' '}
          {filtered.length === 1
            ? strings.sidebar.countSingular
            : strings.sidebar.countPlural}
        </div>
      )}

      {grouped.length === 0 ? (
        <div className="text-muted-foreground py-6 text-center">
          <Users className="mx-auto mb-2 h-8 w-8 opacity-30" />
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
      <div className="text-meta text-muted-foreground flex items-center gap-2 px-1 font-medium">
        <KeyIcon className="h-3 w-3 shrink-0" />
        <span className="break-all font-mono normal-case tracking-normal">
          {group.key}
        </span>
        {joined && (
          <span
            style={{ fontSize: 10 }}
            className="bg-muted text-foreground shrink-0 rounded-full px-1.5 font-semibold tracking-wider"
          >
            {strings.badges.joined}
          </span>
        )}
        {groupIsPreview && (
          <span
            style={{ fontSize: 10 }}
            className="border-border text-muted-foreground shrink-0 rounded-full border px-1.5 font-semibold tracking-wider"
          >
            {strings.badges.preview}
          </span>
        )}
        <span className="ml-auto shrink-0 font-medium normal-case tracking-normal">
          {group.sessions.length}
        </span>
      </div>
      {group.sessions.map((s) => {
        const phase = s.preview ? previewPhaseLabel(s.preview) : null
        // Previews served by an operator that doesn't report their phase are assumed to be up,
        // which is what the sidebar showed before the phase was available at all.
        const tone = s.preview ? previewPhaseTone(s.preview) : 'live'
        return (
          <SessionRow
            key={s.id}
            selected={selectedId === s.id}
            onClick={() => onSelect(s.id)}
            lead={
              isPreviewSession(s) ? (
                <PreviewBadge tone={tone} label={phase} />
              ) : (
                <Avatar name={s.owner.username} seed={s.owner.k8sUsername} />
              )
            }
            target={
              s.target ? `${s.target.kind}/${s.target.name}` : 'targetless'
            }
            meta={[
              isPreviewSession(s) ? 'preview env' : firstName(s.owner.username),
              ...(phase
                ? [<PhaseLabel key="phase" tone={tone} label={phase} />]
                : []),
              s.namespace,
              relativeTimeFromIso(s.createdAt),
            ]}
            leftStrip={joined ? 'hsl(var(--primary))' : undefined}
          />
        )
      })}
    </div>
  )
}

const PREVIEW_BADGE_TONE: Record<PreviewTone, string> = {
  live: 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-300',
  pending: 'bg-amber-500/15 text-amber-600 dark:text-amber-300',
  idle: 'bg-muted text-muted-foreground',
  failed: 'bg-destructive/15 text-destructive',
}

function PreviewBadge({
  tone,
  label,
}: {
  tone: PreviewTone
  label: string | null
}) {
  return (
    <span
      className={`inline-flex items-center justify-center rounded-full ${PREVIEW_BADGE_TONE[tone]}`}
      style={{ width: 26, height: 26 }}
      title={label ? `preview environment (${label})` : 'preview environment'}
    >
      <FlaskConical className="h-3.5 w-3.5" />
    </span>
  )
}

function PhaseLabel({ tone, label }: { tone: PreviewTone; label: string }) {
  return (
    <span className={tone === 'failed' ? 'text-destructive font-medium' : ''}>
      {label}
    </span>
  )
}
