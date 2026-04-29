import { useMemo, useState } from 'react'
import { Button, Input, cn } from '@metalbear/ui'
import { Box, ChevronRight, Search, Users } from 'lucide-react'
import type { OperatorSessionSummary } from '../types'

interface OperatorListProps {
  sessions: OperatorSessionSummary[]
  selectedId: string | null
  onSelect: (id: string) => void
}

interface TargetGroup {
  targetLabel: string
  namespace: string
  sessions: OperatorSessionSummary[]
}

function groupByTarget(sessions: OperatorSessionSummary[]): TargetGroup[] {
  const map = new Map<string, TargetGroup>()
  for (const s of sessions) {
    const targetLabel = s.target ? `${s.target.kind}/${s.target.name}` : 'targetless'
    const k = `${s.namespace}\n${targetLabel}`
    const existing = map.get(k)
    if (existing) existing.sessions.push(s)
    else map.set(k, { targetLabel, namespace: s.namespace, sessions: [s] })
  }
  return Array.from(map.values()).sort((a, b) => b.sessions.length - a.sessions.length)
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

export default function OperatorList({ sessions, selectedId, onSelect }: OperatorListProps) {
  const [query, setQuery] = useState('')
  const normalized = query.trim().toLowerCase()
  const filtered = useMemo(
    () => sessions.filter((s) => matchesQuery(s, normalized)),
    [sessions, normalized]
  )
  const groups = useMemo(() => groupByTarget(filtered), [filtered])

  return (
    <div className="flex flex-col gap-2">
      <div className="relative">
        <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground pointer-events-none" />
        <Input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="search by team, user, target, namespace"
          spellCheck={false}
          autoComplete="off"
          className="h-8 pl-8 text-xs"
        />
      </div>

      {sessions.length > 0 && (
        <div className="text-[10px] uppercase tracking-wider text-muted-foreground font-semibold px-1">
          {filtered.length} live operator session{filtered.length === 1 ? '' : 's'}
        </div>
      )}

      {groups.length === 0 ? (
        <div className="text-center text-muted-foreground py-10">
          <Users className="h-9 w-9 mx-auto mb-3 opacity-30" />
          <p className="text-sm">
            {sessions.length === 0
              ? "No operator sessions yet."
              : "No sessions match your search."}
          </p>
        </div>
      ) : (
        groups.map((g) => (
          <TargetGroupCard
            key={`${g.namespace}/${g.targetLabel}`}
            group={g}
            selectedId={selectedId}
            onSelect={onSelect}
          />
        ))
      )}
    </div>
  )
}

function TargetGroupCard({
  group,
  selectedId,
  onSelect,
}: {
  group: TargetGroup
  selectedId: string | null
  onSelect: (id: string) => void
}) {
  return (
    <div className="rounded-lg border border-border bg-card overflow-hidden">
      <div className="flex items-center gap-2 px-3 py-2 border-b border-border bg-card/60">
        <Box className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
        <div className="min-w-0 flex-1">
          <div className="font-mono text-sm font-semibold truncate">{group.targetLabel}</div>
          <div className="text-[11px] text-muted-foreground truncate">
            {group.namespace} · {group.sessions.length} session
            {group.sessions.length === 1 ? '' : 's'}
          </div>
        </div>
      </div>
      <div className="divide-y divide-border">
        {group.sessions.map((s) => (
          <OperatorSessionRow
            key={s.id}
            session={s}
            selected={s.id === selectedId}
            onSelect={() => onSelect(s.id)}
          />
        ))}
      </div>
    </div>
  )
}

function OperatorSessionRow({
  session,
  selected,
  onSelect,
}: {
  session: OperatorSessionSummary
  selected: boolean
  onSelect: () => void
}) {
  const filterDescription = describeFilter(session.httpFilter)
  return (
    <button
      type="button"
      onClick={onSelect}
      className={cn(
        'w-full text-left px-3 py-2 hover:bg-muted/50 transition-colors flex items-start gap-2',
        selected && 'bg-primary/10'
      )}
    >
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2 text-sm">
          <span className="font-medium truncate">{session.owner.username}</span>
          <span className="text-[11px] text-muted-foreground shrink-0">
            {relativeTime(session.createdAt)}
          </span>
        </div>
        {filterDescription && (
          <div className="text-[11px] text-muted-foreground font-mono truncate mt-0.5">
            {filterDescription}
          </div>
        )}
        <div className="text-[10px] text-muted-foreground/80 font-mono mt-0.5">
          key {session.key}
        </div>
      </div>
      <ChevronRight className="h-3.5 w-3.5 text-muted-foreground shrink-0 mt-1" />
    </button>
  )
}

function describeFilter(f: OperatorSessionSummary['httpFilter']): string {
  if (!f) return ''
  if (f.headerFilter) return `on ${f.headerFilter}`
  if (f.pathFilter) return `path ${f.pathFilter}`
  if (f.allOf?.length) return `${f.allOf.length} filters (all)`
  if (f.anyOf?.length) return `${f.anyOf.length} filters (any)`
  return ''
}
