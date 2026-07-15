import {
  useState,
  useRef,
  useEffect,
  useCallback,
  type PointerEvent as ReactPointerEvent,
} from 'react'
import {
  Button,
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
  Loader,
  SearchInput,
  cn,
} from '@metalbear/ui'
import {
  Activity,
  Cloud,
  Key as KeyIcon,
  Laptop,
  PanelLeftClose,
  PanelLeftOpen,
  Trash2,
} from 'lucide-react'
import type { OperatorSessionSummary, OperatorWatchStatus, SessionInfo } from '../types'
import { strings } from '../strings'
import SessionCard from './SessionCard'
import OperatorList from './OperatorList'

const SIDEBAR_MIN = 240
const SIDEBAR_MAX = 600
const SIDEBAR_DEFAULT = 340
const SIDEBAR_STORAGE_KEY = 'session-monitor-sidebar-width'
const SIDEBAR_HIDDEN_KEY = 'session-monitor-sidebar-hidden'
const FUNNEL_SKELETON_COUNT = 5

function getSavedSidebarWidth(): number {
  try {
    const saved = localStorage.getItem(SIDEBAR_STORAGE_KEY)
    if (saved) {
      const val = parseInt(saved, 10)
      if (val >= SIDEBAR_MIN && val <= SIDEBAR_MAX) return val
    }
  } catch {}
  return SIDEBAR_DEFAULT
}

function getSavedSidebarHidden(): boolean {
  try {
    return localStorage.getItem(SIDEBAR_HIDDEN_KEY) === 'true'
  } catch {}
  return false
}

interface SessionSidebarProps {
  sessions: SessionInfo[]
  selectedId: string | null
  loading: boolean
  onSelect: (id: string) => void
  onKill: (id: string) => void
  onKillAll: () => void
  operatorSessions: OperatorSessionSummary[]
  yoursOperatorSessions: OperatorSessionSummary[]
  allOperatorSessions: OperatorSessionSummary[]
  watchStatus: OperatorWatchStatus | null
  selectedOperatorId: string | null
  onSelectOperator: (id: string) => void
  onConnectOperator: () => void
  joinedKey: string | null
  query: string
  onQueryChange: (query: string) => void
}

const isMac = typeof navigator !== 'undefined' && /Mac/i.test(navigator.userAgent)
const SEARCH_HINT = isMac ? '⌘F' : 'Ctrl F'

export default function SessionSidebar({
  sessions,
  selectedId,
  loading,
  onSelect,
  onKill,
  onKillAll,
  operatorSessions,
  yoursOperatorSessions,
  allOperatorSessions,
  watchStatus,
  selectedOperatorId,
  onSelectOperator,
  onConnectOperator,
  joinedKey,
  query,
  onQueryChange,
}: SessionSidebarProps) {
  const yoursTotal = sessions.length + yoursOperatorSessions.length
  const [sidebarWidth, setSidebarWidth] = useState(getSavedSidebarWidth)
  const [sidebarHidden, setSidebarHidden] = useState(getSavedSidebarHidden)
  const isDraggingRef = useRef(false)
  const [isDragging, setIsDragging] = useState(false)
  const searchRef = useRef<HTMLInputElement>(null)
  const normalizedQuery = query.trim().toLowerCase()

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && (e.key === 'f' || e.key === 'F')) {
        e.preventDefault()
        searchRef.current?.focus()
        searchRef.current?.select()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  const matchesLocal = (s: SessionInfo): boolean => {
    if (!normalizedQuery) return true
    const haystack = [s.target, s.processes.map((p) => p.process_name).join(' '), s.session_id]
      .join(' ')
      .toLowerCase()
    return haystack.includes(normalizedQuery)
  }
  const filteredLocalSessions = sessions.filter(matchesLocal)
  const yoursAfterFilter = filteredLocalSessions.length + yoursOperatorSessions.length

  useEffect(() => {
    localStorage.setItem(SIDEBAR_HIDDEN_KEY, sidebarHidden ? 'true' : 'false')
  }, [sidebarHidden])

  const handlePointerDown = useCallback((e: ReactPointerEvent<HTMLDivElement>) => {
    e.preventDefault()
    e.currentTarget.setPointerCapture(e.pointerId)
    isDraggingRef.current = true
    setIsDragging(true)
  }, [])

  const handlePointerMove = useCallback((e: ReactPointerEvent<HTMLDivElement>) => {
    if (!isDraggingRef.current) return
    const newWidth = Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, e.clientX))
    setSidebarWidth(newWidth)
  }, [])

  const handlePointerUp = useCallback((e: ReactPointerEvent<HTMLDivElement>) => {
    if (!isDraggingRef.current) return
    e.currentTarget.releasePointerCapture(e.pointerId)
    isDraggingRef.current = false
    setIsDragging(false)
    localStorage.setItem(SIDEBAR_STORAGE_KEY, String(e.clientX))
  }, [])

  if (sidebarHidden) {
    return (
      <Button
        variant="ghost"
        onClick={() => setSidebarHidden(false)}
        title={strings.sidebar.showSidebar}
        className="border-border text-muted-foreground hover:text-foreground h-full w-8 shrink-0 rounded-none border-r"
      >
        <PanelLeftOpen className="h-4 w-4" />
      </Button>
    )
  }

  const teamUnavailable = watchStatus?.status === 'unavailable'
  const errorMessage = watchStatus?.status === 'error' ? watchStatus.message : ''
  // A transient 503 from the operator's status APIService (typically the pod
  // restarting) shouldn't read as a hard failure: keep the last-known sessions
  // on screen and show a soft "reconnecting" hint instead of the error box.
  const teamReconnecting = /503|service unavailable/i.test(errorMessage)
  const teamError = watchStatus?.status === 'error' && !teamReconnecting
  const teamConnecting = watchStatus?.status === 'not_started'

  return (
    <>
      <div
        className="border-border surface-inset relative flex shrink-0 flex-col gap-4 overflow-y-auto border-r p-3"
        style={{ width: sidebarWidth }}
      >
        <div className="relative">
          <SearchInput
            ref={searchRef}
            value={query}
            onChange={(e) => onQueryChange(e.target.value)}
            onClear={() => onQueryChange('')}
            placeholder={strings.app.searchPlaceholder}
            className="h-8 pr-12 text-xs"
          />
          {!query && (
            <kbd className="border-border bg-muted/50 text-muted-foreground pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 select-none rounded border px-1.5 py-0.5 font-mono text-[10px] leading-none">
              {SEARCH_HINT}
            </kbd>
          )}
        </div>

        <SectionHeader
          icon={<Laptop className="h-3.5 w-3.5" />}
          label="Yours"
          count={yoursTotal}
          actions={
            <>
              {sessions.length > 0 && (
                <Dialog>
                  <DialogTrigger asChild>
                    <Button
                      variant="ghost"
                      size="icon"
                      title={strings.sidebar.killAllTooltip}
                      className="text-muted-foreground hover:text-destructive hover:bg-destructive/10 h-6 w-6"
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  </DialogTrigger>
                  <DialogContent>
                    <DialogHeader>
                      <DialogTitle>{strings.sidebar.killAllTitle}</DialogTitle>
                      <DialogDescription>
                        {strings.sidebar.killAllDescription(sessions.length)}
                      </DialogDescription>
                    </DialogHeader>
                    <DialogFooter>
                      <DialogClose asChild>
                        <Button variant="outline" size="sm">
                          {strings.sidebar.cancel}
                        </Button>
                      </DialogClose>
                      <DialogClose asChild>
                        <Button variant="destructive" size="sm" onClick={onKillAll}>
                          <Trash2 className="mr-1.5 h-3.5 w-3.5" />
                          {strings.sidebar.killAllButton}
                        </Button>
                      </DialogClose>
                    </DialogFooter>
                  </DialogContent>
                </Dialog>
              )}
              <Button
                variant="ghost"
                size="icon"
                onClick={() => setSidebarHidden(true)}
                title={strings.sidebar.hideSidebar}
                className="text-muted-foreground hover:text-foreground h-6 w-6"
              >
                <PanelLeftClose className="h-4 w-4" />
              </Button>
            </>
          }
        />

        <div className="flex flex-col gap-2">
          {loading ? (
            <div className="flex items-center justify-center py-8">
              <Loader size="lg" />
            </div>
          ) : yoursTotal === 0 ? (
            <div className="text-muted-foreground py-6 text-center">
              <Activity className="mx-auto mb-2 h-8 w-8 opacity-30" />
              <p className="mb-1 text-sm">{strings.sidebar.emptyTitle}</p>
              <p className="text-xs opacity-70">{strings.sidebar.emptyBody}</p>
            </div>
          ) : yoursAfterFilter === 0 ? (
            <div className="text-muted-foreground py-4 text-center">
              <p className="text-xs">No sessions match your search.</p>
            </div>
          ) : (
            <>
              {filteredLocalSessions.length > 0 && (
                <LocalSessionsByKey
                  sessions={filteredLocalSessions}
                  selectedId={selectedId}
                  onSelect={onSelect}
                  onKill={onKill}
                  allOperatorSessions={allOperatorSessions}
                  joinedKey={joinedKey}
                />
              )}
              {yoursOperatorSessions.length > 0 && (
                <>
                  {filteredLocalSessions.length > 0 && (
                    <div className="text-meta text-muted-foreground -mb-1 mt-2 px-1 font-medium">
                      Cluster-side
                    </div>
                  )}
                  <OperatorList
                    sessions={yoursOperatorSessions}
                    selectedId={selectedOperatorId}
                    onSelect={onSelectOperator}
                    joinedKey={joinedKey}
                    query={query}
                    emptyLabel="No cluster-side sessions for you yet."
                  />
                </>
              )}
            </>
          )}
        </div>

        <SectionHeader
          icon={<Cloud className="h-3.5 w-3.5" />}
          label="Team"
          count={watchStatus?.status === 'watching' ? operatorSessions.length : null}
        />

        {teamUnavailable ? (
          <FunnelInline onConnect={onConnectOperator} />
        ) : teamError ? (
          <div className="bg-destructive/10 border-destructive/40 rounded-lg border px-3 py-2">
            <div className="text-destructive text-xs font-semibold">Operator error</div>
            <div className="text-meta text-destructive/80 mt-0.5 break-words">
              {errorMessage || 'Could not reach the operator.'}
            </div>
          </div>
        ) : teamConnecting ? (
          <div className="text-muted-foreground py-4 text-center text-xs">
            Connecting to operator…
          </div>
        ) : (
          <>
            {teamReconnecting && (
              <div className="text-meta text-muted-foreground -mb-1 flex items-center gap-1.5 px-1">
                <Loader size="sm" />
                <span>Reconnecting to operator…</span>
              </div>
            )}
            <OperatorList
              sessions={operatorSessions}
              selectedId={selectedOperatorId}
              onSelect={onSelectOperator}
              joinedKey={joinedKey}
              query={query}
            />
          </>
        )}
      </div>

      <div
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        className={cn(
          'group relative w-1 shrink-0 cursor-col-resize touch-none transition-colors',
          isDragging ? 'bg-border' : 'hover:bg-border bg-transparent',
        )}
      >
        <div className="absolute inset-y-0 -left-1 -right-1 cursor-col-resize" />
      </div>
    </>
  )
}

interface LocalSessionsByKeyProps {
  sessions: SessionInfo[]
  selectedId: string | null
  onSelect: (id: string) => void
  onKill: (id: string) => void
  allOperatorSessions: OperatorSessionSummary[]
  joinedKey: string | null
}

const NO_KEY_GROUP = '__no_key__'

function LocalSessionsByKey({
  sessions,
  selectedId,
  onSelect,
  onKill,
  allOperatorSessions,
  joinedKey,
}: LocalSessionsByKeyProps) {
  const groups = new Map<string, SessionInfo[]>()
  for (const s of sessions) {
    const key = s.key ?? NO_KEY_GROUP
    const arr = groups.get(key)
    if (arr) arr.push(s)
    else groups.set(key, [s])
  }
  const orderedEntries = Array.from(groups.entries()).sort(([a], [b]) => {
    if (a === joinedKey) return -1
    if (b === joinedKey) return 1
    if (a === NO_KEY_GROUP) return 1
    if (b === NO_KEY_GROUP) return -1
    return a.localeCompare(b)
  })

  return (
    <div className="flex flex-col gap-2.5">
      {orderedEntries.map(([key, groupSessions]) => {
        const isJoinedGroup = key !== NO_KEY_GROUP && key === joinedKey
        return (
          <div key={key} className="flex flex-col gap-1">
            <div className="text-meta text-muted-foreground flex items-center gap-2 px-1 font-medium">
              <KeyIcon className="h-3 w-3 shrink-0" />
              <span className="break-all font-mono normal-case tracking-normal">
                {key === NO_KEY_GROUP ? 'No session key' : key}
              </span>
              {isJoinedGroup && (
                <span
                  style={{ fontSize: 10 }}
                  className="bg-muted text-foreground shrink-0 rounded-full px-1.5 font-semibold tracking-wider"
                >
                  JOINED
                </span>
              )}
              <span className="ml-auto shrink-0 font-medium normal-case tracking-normal">
                {groupSessions.length}
              </span>
            </div>
            {groupSessions.map((s) => {
              const owner = allOperatorSessions.find((o) => o.id === s.session_id)?.owner ?? null
              const isJoined = !!joinedKey && !!s.key && s.key === joinedKey
              return (
                <SessionCard
                  key={s.session_id}
                  session={s}
                  selected={s.session_id === selectedId}
                  onSelect={() => onSelect(s.session_id === selectedId ? '' : s.session_id)}
                  onKill={() => onKill(s.session_id)}
                  owner={owner}
                  joined={isJoined}
                />
              )
            })}
          </div>
        )
      })}
    </div>
  )
}

function SectionHeader({
  icon,
  label,
  count,
  actions,
}: {
  icon: React.ReactNode
  label: string
  count?: number | null
  actions?: React.ReactNode
}) {
  return (
    <div className="flex items-center justify-between px-1">
      <div className="text-section text-foreground flex items-center gap-1.5">
        <span className="text-muted-foreground">{icon}</span>
        {label}
        {count !== null && count !== undefined && count > 0 && (
          <span className="text-muted-foreground">· {count}</span>
        )}
      </div>
      {actions && <div className="flex items-center gap-1">{actions}</div>}
    </div>
  )
}

function FunnelInline({ onConnect }: { onConnect: () => void }) {
  return (
    <div className="flex flex-col gap-2">
      <button
        type="button"
        onClick={onConnect}
        className="border-border hover:border-primary hover:bg-muted/50 flex w-full items-center gap-2 rounded-lg border border-dashed px-3 py-2 text-left transition-colors"
      >
        <span className="bg-muted-foreground/60 h-1.5 w-1.5 shrink-0 rounded-full" />
        <span className="text-muted-foreground flex-1 text-xs">
          Showing only your sessions.{' '}
          <span className="text-primary font-semibold">Connect operator →</span>
        </span>
      </button>
      <div className="text-meta text-muted-foreground px-1">
        0 sessions · operator not connected
      </div>
      <div
        aria-hidden
        className="flex flex-col gap-1.5"
        style={{ filter: 'blur(2.4px)', opacity: 0.45 }}
      >
        {Array.from({ length: FUNNEL_SKELETON_COUNT }, (_, i) => (
          <div
            key={i}
            className="bg-card border-border flex h-12 items-center gap-2.5 rounded-lg border px-3"
          >
            <div className="bg-muted h-5 w-5 rounded-full" />
            <div className="flex flex-1 flex-col gap-1">
              <div className="bg-muted h-2 w-[70%] rounded" />
              <div className="bg-muted h-1.5 w-[40%] rounded opacity-70" />
            </div>
            <div className="bg-primary/30 h-3.5 w-7 rounded" />
          </div>
        ))}
      </div>
    </div>
  )
}
