import { useState, useEffect, useCallback, useRef } from 'react'
import { MirrordIcon } from '@metalbear/ui'
import { Loader, cn } from '@metalbear/ui'
import { Activity, Sun, Moon, PanelLeftClose, PanelLeftOpen } from 'lucide-react'
import type { SessionInfo, WsMessage } from './types'
import SessionCard from './SessionCard'
import EventStream from './EventStream'

const WS_RECONNECT_INTERVAL = 3000

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

export default function App() {
  const [sessions, setSessions] = useState<SessionInfo[]>([])
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [connected, setConnected] = useState(false)
  const [loading, setLoading] = useState(true)
  const [sidebarWidth, setSidebarWidth] = useState(getSavedSidebarWidth)
  const [sidebarHidden, setSidebarHidden] = useState(getSavedSidebarHidden)
  const [isDragging, setIsDragging] = useState(false)
  const sidebarRef = useRef<HTMLDivElement>(null)
  const sidebarWidthRef = useRef(getSavedSidebarWidth())
  const [isDarkMode, setIsDarkMode] = useState(() => {
    const saved = localStorage.getItem('session-monitor-theme')
    if (saved) return saved === 'dark'
    return window.matchMedia('(prefers-color-scheme: dark)').matches
  })

  useEffect(() => {
    document.documentElement.classList.toggle('dark', isDarkMode)
    localStorage.setItem('session-monitor-theme', isDarkMode ? 'dark' : 'light')
  }, [isDarkMode])

  // Sidebar drag resize
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

  // Initial fetch
  useEffect(() => {
    fetch('/api/sessions')
      .then((r) => {
        if (!r.ok) {
          console.error('Failed to fetch sessions:', r.status, r.statusText)
          return [] as SessionInfo[]
        }
        return r.json()
      })
      .then((data: SessionInfo[]) => {
        setSessions(data)
        setLoading(false)
      })
      .catch((err) => {
        console.error(err)
        setLoading(false)
      })
  }, [])

  // WebSocket for live updates with reconnection
  useEffect(() => {
    let ws: WebSocket | null = null
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null
    let stopped = false

    function connect() {
      if (stopped) return
      const wsUrl = `${window.location.protocol === 'https:' ? 'wss:' : 'ws:'}//${window.location.host}/ws`
      ws = new WebSocket(wsUrl)

      ws.onopen = () => setConnected(true)
      ws.onclose = () => {
        setConnected(false)
        if (!stopped) {
          reconnectTimer = setTimeout(connect, WS_RECONNECT_INTERVAL)
        }
      }

      ws.onmessage = (e) => {
        let msg: WsMessage
        try {
          msg = JSON.parse(e.data)
        } catch (err) {
          console.error('Failed to parse WebSocket message:', err)
          return
        }
        if (msg.type === 'session_added') {
          const session = msg.session
          setSessions((prev) => {
            if (prev.find((s) => s.session_id === session.session_id)) return prev
            return [...prev, session]
          })
        } else if (msg.type === 'session_removed') {
          const removedId = msg.session_id
          setSessions((prev) => prev.filter((s) => s.session_id !== removedId))
          setSelectedId((prev) => (prev === removedId ? null : prev))
        }
      }
    }

    connect()
    return () => {
      stopped = true
      if (reconnectTimer) clearTimeout(reconnectTimer)
      ws?.close()
    }
  }, [])

  const handleKill = useCallback(async (id: string) => {
    await fetch(`/api/sessions/${id}/kill`, { method: 'POST' })
  }, [])

  const selected = sessions.find((s) => s.session_id === selectedId)

  return (
    <div className="min-h-screen bg-background text-foreground">
      <header className="relative dark:bg-dark-background bg-white/80 backdrop-blur-sm border-b border-border dark:border-transparent">
        <div className="absolute bottom-0 left-0 right-0 h-[2px] bg-gradient-to-r from-transparent via-primary to-transparent opacity-40" />
        <div className="px-6">
          <div className="flex items-center justify-between h-14 text-foreground">
            <div className="flex items-center gap-3">
              <img
                src={MirrordIcon}
                alt="mirrord"
                className="w-8 h-8 dark:invert"
              />
              <span className="font-semibold text-lg">mirrord</span>
              <span className="opacity-30">|</span>
              <span className="text-sm font-medium opacity-80">
                Session Monitor
              </span>
            </div>
            <div className="flex items-center gap-3">
              <div className="flex items-center gap-2">
                <div
                  className={cn(
                    'h-2 w-2 rounded-full',
                    connected ? 'bg-green-500' : 'bg-red-500'
                  )}
                />
                <span className="text-sm opacity-60">
                  {connected ? 'Connected' : 'Disconnected'}
                </span>
              </div>
              <span className="opacity-20">|</span>
              <div className="flex items-center gap-1.5 text-sm opacity-60">
                <Activity className="h-3.5 w-3.5" />
                <span>
                  {sessions.length} session{sessions.length !== 1 ? 's' : ''}
                </span>
              </div>
              <span className="opacity-20">|</span>
              <button
                onClick={() => setIsDarkMode(!isDarkMode)}
                className="p-1.5 rounded-md transition-colors hover:bg-foreground/10 opacity-60 hover:opacity-100"
                title={isDarkMode ? 'Switch to light mode' : 'Switch to dark mode'}
              >
                {isDarkMode ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
              </button>
            </div>
          </div>
        </div>
      </header>

      <div className="flex h-[calc(100vh-57px)]">
        {!sidebarHidden && (
        <>
        <div
          ref={sidebarRef}
          className="border-r border-border overflow-y-auto p-3 space-y-2 shrink-0 relative"
          style={{ width: sidebarWidth }}
        >
          <div className="flex justify-end mb-1">
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
                onSelect={() => setSelectedId(s.session_id === selectedId ? null : s.session_id)}
                onKill={() => handleKill(s.session_id)}
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
        )}

        {sidebarHidden && (
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
        )}

        <div className="flex-1 overflow-hidden">
          {selected ? (
            <EventStream session={selected} />
          ) : (
            <div className="flex flex-col items-center justify-center h-full text-muted-foreground gap-2">
              <Activity className="h-8 w-8 opacity-30" />
              <p>Select a session to view live events</p>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
