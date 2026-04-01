import { useState, useEffect, useCallback } from 'react'
import { MirrordIcon } from '@metalbear/ui'
import { cn } from '@metalbear/ui'
import { Sun, Moon, Activity } from 'lucide-react'
import type { SessionInfo, WsMessage } from './types'
import SessionSidebar from './SessionSidebar'
import SessionDetail from './SessionDetail'
import StatusBar from './StatusBar'

const WS_RECONNECT_INTERVAL = 3000

export default function App() {
  const [sessions, setSessions] = useState<SessionInfo[]>([])
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [connected, setConnected] = useState(false)
  const [loading, setLoading] = useState(true)
  const [isDarkMode, setIsDarkMode] = useState(() => {
    const saved = localStorage.getItem('session-monitor-theme')
    if (saved) return saved === 'dark'
    return window.matchMedia('(prefers-color-scheme: dark)').matches
  })

  useEffect(() => {
    document.documentElement.classList.toggle('dark', isDarkMode)
    localStorage.setItem('session-monitor-theme', isDarkMode ? 'dark' : 'light')
  }, [isDarkMode])

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

  const handleSelect = useCallback((id: string) => {
    setSelectedId((prev) => (prev === id || id === '' ? null : id))
  }, [])

  const selected = sessions.find((s) => s.session_id === selectedId)

  return (
    <div className="h-screen flex flex-col bg-background text-foreground">
      {/* Header */}
      <header className="dark:bg-dark-background bg-white/80 backdrop-blur-sm border-b border-border dark:border-transparent shrink-0">
        <div className="px-6">
          <div className="flex items-center justify-between h-12 text-foreground">
            <div className="flex items-center gap-3">
              <img
                src={MirrordIcon}
                alt="mirrord"
                className="w-7 h-7 dark:invert"
              />
              <span className="font-semibold text-base">mirrord</span>
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
                <span className="text-xs opacity-60">
                  {connected ? 'Connected' : 'Disconnected'}
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

      {/* Main content */}
      <div className="flex flex-1 overflow-hidden">
        <SessionSidebar
          sessions={sessions}
          selectedId={selectedId}
          loading={loading}
          onSelect={handleSelect}
          onKill={handleKill}
        />

        <div className="flex-1 overflow-hidden">
          {selected ? (
            <SessionDetail session={selected} />
          ) : (
            <div className="flex flex-col items-center justify-center h-full text-muted-foreground gap-2">
              <Activity className="h-8 w-8 opacity-30" />
              <p className="text-sm">Select a session to view live events</p>
            </div>
          )}
        </div>
      </div>

      {/* Status bar */}
      <StatusBar wsConnected={connected} session={selected} />
    </div>
  )
}
