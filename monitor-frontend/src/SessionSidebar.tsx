import { useState, useRef, useEffect } from 'react'
import { Loader, cn, Button } from '@metalbear/ui'
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter, DialogClose, DialogTrigger } from '@metalbear/ui'
import { Activity, PanelLeftClose, PanelLeftOpen, Trash2 } from 'lucide-react'
import type { SessionInfo } from './types'
import SessionCard from './SessionCard'

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

interface SessionSidebarProps {
  sessions: SessionInfo[]
  selectedId: string | null
  loading: boolean
  onSelect: (id: string) => void
  onKill: (id: string) => void
  onKillAll: () => void
}

export default function SessionSidebar({ sessions, selectedId, loading, onSelect, onKill, onKillAll }: SessionSidebarProps) {
  const [sidebarWidth, setSidebarWidth] = useState(getSavedSidebarWidth)
  const [sidebarHidden, setSidebarHidden] = useState(getSavedSidebarHidden)
  const [isDragging, setIsDragging] = useState(false)
  const sidebarWidthRef = useRef(getSavedSidebarWidth())

  useEffect(() => {
    if (!isDragging) return

    const handleMouseMove = (e: MouseEvent) => {
      e.preventDefault()
      const newWidth = Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, e.clientX))
      setSidebarWidth(newWidth)
      sidebarWidthRef.current = newWidth
    }

    const handleMouseUp = () => {
      setIsDragging(false)
      document.body.style.cursor = ''
      document.body.style.userSelect = ''
      localStorage.setItem(SIDEBAR_STORAGE_KEY, String(sidebarWidthRef.current))
    }

    document.body.style.cursor = 'col-resize'
    document.body.style.userSelect = 'none'
    window.addEventListener('mousemove', handleMouseMove)
    window.addEventListener('mouseup', handleMouseUp)

    return () => {
      window.removeEventListener('mousemove', handleMouseMove)
      window.removeEventListener('mouseup', handleMouseUp)
      document.body.style.cursor = ''
      document.body.style.userSelect = ''
    }
  }, [isDragging])

  if (sidebarHidden) {
    return (
      <button
        onClick={() => {
          setSidebarHidden(false)
          localStorage.setItem(SIDEBAR_HIDDEN_KEY, 'false')
        }}
        className="shrink-0 w-8 border-r border-border flex items-center justify-center hover:bg-muted transition-colors text-muted-foreground hover:text-foreground"
        title="Show sidebar"
      >
        <PanelLeftOpen className="h-4 w-4" />
      </button>
    )
  }

  return (
    <>
      <div
        className="border-r border-border overflow-y-auto p-3 space-y-2 shrink-0 relative bg-card/20"
        style={{ width: sidebarWidth }}
      >
        <div className="flex items-center justify-between mb-1">
          <span className="text-xs text-muted-foreground">
            {sessions.length} session{sessions.length !== 1 ? 's' : ''}
          </span>
          <div className="flex items-center gap-1">
            {sessions.length > 0 && (
              <Dialog>
                <DialogTrigger asChild>
                  <button
                    className="p-1.5 rounded-md hover:bg-destructive/10 text-muted-foreground hover:text-destructive transition-colors"
                    title="Kill all sessions"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                </DialogTrigger>
                <DialogContent>
                  <DialogHeader>
                    <DialogTitle>Kill all sessions?</DialogTitle>
                    <DialogDescription>
                      This will terminate {sessions.length} active mirrord session{sessions.length !== 1 ? 's' : ''}. This action cannot be undone.
                    </DialogDescription>
                  </DialogHeader>
                  <DialogFooter>
                    <DialogClose asChild>
                      <Button variant="outline" size="sm">Cancel</Button>
                    </DialogClose>
                    <DialogClose asChild>
                      <Button variant="destructive" size="sm" onClick={onKillAll}>
                        <Trash2 className="h-3.5 w-3.5 mr-1.5" />
                        Kill All
                      </Button>
                    </DialogClose>
                  </DialogFooter>
                </DialogContent>
              </Dialog>
            )}
            <button
              onClick={() => {
                setSidebarHidden(true)
                localStorage.setItem(SIDEBAR_HIDDEN_KEY, 'true')
              }}
              className="p-1.5 rounded-md hover:bg-muted text-muted-foreground hover:text-foreground transition-colors"
              title="Hide sidebar"
            >
              <PanelLeftClose className="h-4 w-4" />
            </button>
          </div>
        </div>
        {loading ? (
          <div className="flex items-center justify-center py-12">
            <Loader size="lg" />
          </div>
        ) : sessions.length === 0 ? (
          <div className="text-center text-muted-foreground py-12">
            <Activity className="h-10 w-10 mx-auto mb-3 opacity-30" />
            <p className="text-base mb-1">No active sessions</p>
            <p className="text-sm opacity-70">Start mirrord to see sessions here</p>
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
      </div>

      <div
        className={cn(
          'w-1 shrink-0 cursor-col-resize transition-colors relative group',
          isDragging ? 'bg-primary' : 'bg-transparent hover:bg-primary/50'
        )}
        onMouseDown={(e) => {
          e.preventDefault()
          setIsDragging(true)
        }}
      >
        <div className={cn(
          'absolute inset-y-0 -left-1 -right-1',
          'cursor-col-resize'
        )} />
      </div>
    </>
  )
}
