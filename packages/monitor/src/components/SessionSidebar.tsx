import { useState, useRef, useEffect, useCallback, type PointerEvent as ReactPointerEvent } from 'react'
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
  cn,
} from '@metalbear/ui'
import { Activity, Cloud, Laptop, PanelLeftClose, PanelLeftOpen, Trash2 } from 'lucide-react'
import type { OperatorSessionSummary, OperatorWatchStatus, SessionInfo } from '../types'
import { strings } from '../strings'
import SessionCard from './SessionCard'
import OperatorList from './OperatorList'

const SIDEBAR_MIN = 240
const SIDEBAR_MAX = 600
const SIDEBAR_DEFAULT = 340
const SIDEBAR_STORAGE_KEY = 'session-monitor-sidebar-width'
const SIDEBAR_HIDDEN_KEY = 'session-monitor-sidebar-hidden'

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
}

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
}: SessionSidebarProps) {
  const yoursTotal = sessions.length + yoursOperatorSessions.length
  const [sidebarWidth, setSidebarWidth] = useState(getSavedSidebarWidth)
  const [sidebarHidden, setSidebarHidden] = useState(getSavedSidebarHidden)
  const isDraggingRef = useRef(false)
  const [isDragging, setIsDragging] = useState(false)
  const normalizedQuery = query.trim().toLowerCase()

  const matchesLocal = (s: SessionInfo): boolean => {
    if (!normalizedQuery) return true
    const haystack = [
      s.target,
      s.processes.map((p) => p.process_name).join(' '),
      s.session_id,
    ]
      .join(' ')
      .toLowerCase()
    return haystack.includes(normalizedQuery)
  }
  const filteredLocalSessions = sessions.filter(matchesLocal)
  const yoursAfterFilter =
    filteredLocalSessions.length +
    yoursOperatorSessions.length

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
        className="shrink-0 w-8 h-full rounded-none border-r border-border text-muted-foreground hover:text-foreground"
      >
        <PanelLeftOpen className="h-4 w-4" />
      </Button>
    )
  }

  const teamUnavailable = watchStatus?.status === 'unavailable'
  const teamError = watchStatus?.status === 'error'
  const teamConnecting = watchStatus?.status === 'not_started'

  return (
    <>
      <div
        className="border-r border-border overflow-y-auto p-3 shrink-0 relative surface-inset flex flex-col gap-4"
        style={{ width: sidebarWidth }}
      >
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
                      className="h-6 w-6 text-muted-foreground hover:text-destructive hover:bg-destructive/10"
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
                          <Trash2 className="h-3.5 w-3.5 mr-1.5" />
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
                className="h-6 w-6 text-muted-foreground hover:text-foreground"
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
            <div className="text-center text-muted-foreground py-6">
              <Activity className="h-8 w-8 mx-auto mb-2 opacity-30" />
              <p className="text-sm mb-1">{strings.sidebar.emptyTitle}</p>
              <p className="text-xs opacity-70">{strings.sidebar.emptyBody}</p>
            </div>
          ) : yoursAfterFilter === 0 ? (
            <div className="text-center text-muted-foreground py-4">
              <p className="text-xs">No sessions match your search.</p>
            </div>
          ) : (
            <>
              {filteredLocalSessions.length > 0 && (
                <>
                  <div className="text-meta font-medium text-muted-foreground px-1 -mb-1">
                    Live on this machine
                  </div>
                  {filteredLocalSessions.map((s) => {
                    const owner =
                      allOperatorSessions.find((o) => o.id === s.session_id)
                        ?.owner ?? null
                    const isJoined =
                      !!joinedKey && !!s.key && s.key === joinedKey
                    return (
                      <SessionCard
                        key={s.session_id}
                        session={s}
                        selected={s.session_id === selectedId}
                        onSelect={() =>
                          onSelect(
                            s.session_id === selectedId ? '' : s.session_id
                          )
                        }
                        onKill={() => onKill(s.session_id)}
                        owner={owner}
                        joined={isJoined}
                      />
                    )
                  })}
                </>
              )}
              {yoursOperatorSessions.length > 0 && (
                <>
                  {filteredLocalSessions.length > 0 && (
                    <div className="text-meta font-medium text-muted-foreground px-1 mt-2 -mb-1">
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
          <div className="px-3 py-2 rounded-lg bg-destructive/10 border border-destructive/40">
            <div className="text-xs font-semibold text-destructive">Operator error</div>
            <div className="text-meta text-destructive/80 mt-0.5 break-words">
              {watchStatus?.status === 'error' ? watchStatus.message || 'Could not reach the operator.' : ''}
            </div>
          </div>
        ) : teamConnecting ? (
          <div className="text-xs text-muted-foreground py-4 text-center">
            Connecting to operator…
          </div>
        ) : (
          <OperatorList
            sessions={operatorSessions}
            selectedId={selectedOperatorId}
            onSelect={onSelectOperator}
            joinedKey={joinedKey}
            query={query}
          />
        )}
      </div>

      <div
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        className={cn(
          'w-1 shrink-0 cursor-col-resize transition-colors relative group touch-none',
          isDragging ? 'bg-border' : 'bg-transparent hover:bg-border'
        )}
      >
        <div className="absolute inset-y-0 -left-1 -right-1 cursor-col-resize" />
      </div>
    </>
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
      <div className="flex items-center gap-1.5 text-section text-foreground">
        <span className="text-muted-foreground">{icon}</span>
        {label}
        {count != null && count > 0 && (
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
        className="w-full text-left flex items-center gap-2 px-3 py-2 rounded-lg border border-dashed border-border hover:border-primary hover:bg-muted/50 transition-colors"
      >
        <span className="w-1.5 h-1.5 rounded-full bg-muted-foreground/60 shrink-0" />
        <span className="flex-1 text-xs text-muted-foreground">
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
        {[0, 1, 2, 3, 4].map((i) => (
          <div
            key={i}
            className="h-12 rounded-lg bg-card border border-border flex items-center gap-2.5 px-3"
          >
            <div className="w-5 h-5 rounded-full bg-muted" />
            <div className="flex flex-col gap-1 flex-1">
              <div className="h-2 w-[70%] rounded bg-muted" />
              <div className="h-1.5 w-[40%] rounded bg-muted opacity-70" />
            </div>
            <div className="w-7 h-3.5 rounded bg-primary/30" />
          </div>
        ))}
      </div>
    </div>
  )
}
