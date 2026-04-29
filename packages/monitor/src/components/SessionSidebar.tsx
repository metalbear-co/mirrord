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
import { Activity, PanelLeftClose, PanelLeftOpen, Trash2 } from 'lucide-react'
import type { OperatorSessionSummary, OperatorWatchStatus, SessionInfo } from '../types'
import { strings } from '../strings'
import SessionCard from './SessionCard'
import OperatorList from './OperatorList'

const SIDEBAR_MIN = 240
const SIDEBAR_MAX = 600
const SIDEBAR_DEFAULT = 320
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

type SidebarTab = 'mine' | 'team'

interface SessionSidebarProps {
  sessions: SessionInfo[]
  selectedId: string | null
  loading: boolean
  onSelect: (id: string) => void
  onKill: (id: string) => void
  onKillAll: () => void
  operatorSessions: OperatorSessionSummary[]
  watchStatus: OperatorWatchStatus | null
  selectedOperatorId: string | null
  onSelectOperator: (id: string) => void
  onConnectOperator: () => void
  activeTab: SidebarTab
  onActiveTabChange: (t: SidebarTab) => void
}

export default function SessionSidebar({
  sessions,
  selectedId,
  loading,
  onSelect,
  onKill,
  onKillAll,
  operatorSessions,
  watchStatus,
  selectedOperatorId,
  onSelectOperator,
  onConnectOperator,
  activeTab,
  onActiveTabChange,
}: SessionSidebarProps) {
  const [sidebarWidth, setSidebarWidth] = useState(getSavedSidebarWidth)
  const [sidebarHidden, setSidebarHidden] = useState(getSavedSidebarHidden)
  const isDraggingRef = useRef(false)
  const [isDragging, setIsDragging] = useState(false)

  useEffect(() => {
    localStorage.setItem(SIDEBAR_HIDDEN_KEY, sidebarHidden ? 'true' : 'false')
  }, [sidebarHidden])

  // Drag-to-resize using pointer capture so we don't need global mouse listeners.
  // With setPointerCapture, the handle element keeps receiving move/up events even
  // when the cursor leaves it — equivalent to a window listener, but scoped.
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

  const countLabel = sessions.length !== 1 ? strings.sidebar.countPlural : strings.sidebar.countSingular

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

  return (
    <>
      <div
        className="border-r border-border overflow-y-auto p-3 shrink-0 relative bg-card/20 flex flex-col gap-2"
        style={{ width: sidebarWidth }}
      >
        <div className="flex items-center gap-1 border-b border-border -mx-3 px-3 pb-2">
          <button
            type="button"
            onClick={() => onActiveTabChange('mine')}
            className={cn(
              'flex-1 text-xs font-semibold py-1.5 px-2 rounded transition-colors',
              activeTab === 'mine'
                ? 'bg-card text-foreground'
                : 'text-muted-foreground hover:text-foreground'
            )}
          >
            Mine{sessions.length > 0 ? ` (${sessions.length})` : ''}
          </button>
          <button
            type="button"
            onClick={() => onActiveTabChange('team')}
            className={cn(
              'flex-1 text-xs font-semibold py-1.5 px-2 rounded transition-colors',
              activeTab === 'team'
                ? 'bg-card text-foreground'
                : 'text-muted-foreground hover:text-foreground'
            )}
          >
            Team{operatorSessions.length > 0 ? ` (${operatorSessions.length})` : ''}
          </button>
        </div>

        {activeTab === 'team' ? (
          watchStatus?.status === 'unavailable' ? (
            <button
              type="button"
              onClick={onConnectOperator}
              className="w-full text-left flex items-center gap-2 px-3 py-2 rounded-lg border border-dashed border-border hover:border-primary hover:bg-muted/50 transition-colors"
            >
              <span className="w-1.5 h-1.5 rounded-full bg-muted-foreground/60" />
              <span className="flex-1 text-xs text-muted-foreground">
                Showing only your sessions.{' '}
                <span className="text-primary font-semibold">Connect operator →</span>
              </span>
            </button>
          ) : watchStatus?.status === 'error' ? (
            <div className="px-3 py-2 rounded-lg bg-destructive/10 border border-destructive/40">
              <div className="text-xs font-semibold text-destructive">Operator error</div>
              <div className="text-[11px] text-destructive/80 mt-0.5 break-words">
                {watchStatus.message || 'Could not reach the operator.'}
              </div>
            </div>
          ) : watchStatus?.status === 'not_started' ? (
            <div className="text-xs text-muted-foreground py-6 text-center">
              Connecting to operator…
            </div>
          ) : (
            <OperatorList
              sessions={operatorSessions}
              selectedId={selectedOperatorId}
              onSelect={onSelectOperator}
            />
          )
        ) : (
        <>
        <div className="flex items-center justify-between mb-1">
          <span className="text-xs text-muted-foreground">
            {sessions.length} {countLabel}
          </span>
          <div className="flex items-center gap-1">
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
                      <Button variant="outline" size="sm">{strings.sidebar.cancel}</Button>
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
          </div>
        </div>
        {loading ? (
          <div className="flex items-center justify-center py-12">
            <Loader size="lg" />
          </div>
        ) : sessions.length === 0 ? (
          <div className="text-center text-muted-foreground py-12">
            <Activity className="h-10 w-10 mx-auto mb-3 opacity-30" />
            <p className="text-base mb-1">{strings.sidebar.emptyTitle}</p>
            <p className="text-sm opacity-70">{strings.sidebar.emptyBody}</p>
          </div>
        ) : (
          sessions.map((s) => (
            <SessionCard
              key={s.session_id}
              session={s}
              selected={s.session_id === selectedId}
              onSelect={() => onSelect(s.session_id === selectedId ? '' : s.session_id)}
              onKill={() => onKill(s.session_id)}
            />
          ))
        )}
        </>
        )}
      </div>

      <div
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerUp}
        className={cn(
          'w-1 shrink-0 cursor-col-resize transition-colors relative group touch-none',
          isDragging ? 'bg-primary' : 'bg-transparent hover:bg-primary/50'
        )}
      >
        <div className="absolute inset-y-0 -left-1 -right-1 cursor-col-resize" />
      </div>
    </>
  )
}
