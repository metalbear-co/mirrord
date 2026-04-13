import { useState, useEffect, useCallback } from 'react'
import { Button, MirrordIcon, cn } from '@metalbear/ui'
import { Sun, Moon, Activity } from 'lucide-react'
import type { SessionInfo, WsMessage } from './types'
import SessionSidebar from './SessionSidebar'
import SessionDetail from './SessionDetail'
import StatusBar from './StatusBar'
import { initAnalytics, trackEvent } from './analytics'
import { api } from './api'
import { strings } from './strings'

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

  // Strip auth token from URL bar after page load (cookie is already set)
  useEffect(() => {
    const params = new URLSearchParams(window.location.search)
    const token = params.get('token')
    if (token) {
      const url = new URL(window.location.href)
      url.searchParams.delete('token')
      window.history.replaceState({}, '', url.toString())
    }
  }, [])

  useEffect(() => {
    document.documentElement.classList.toggle('dark', isDarkMode)
    localStorage.setItem('session-monitor-theme', isDarkMode ? 'dark' : 'light')
  }, [isDarkMode])

  // Initialize PostHog analytics (respects telemetry opt-out)
  useEffect(() => {
    if (sessions.length > 0) {
      const telemetryEnabled = sessions.every(
        s => (s.config as Record<string, unknown>)?.telemetry !== false
      )
      initAnalytics(telemetryEnabled)
    }
  }, [sessions])

  useEffect(() => {
    api.listSessions()
      .then((data) => setSessions(data))
      .catch((err) => console.error(err))
      .finally(() => setLoading(false))
  }, [])

  // WebSocket for live updates with reconnection
  useEffect(() => {
    let ws: WebSocket | null = null
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null
    let stopped = false

    function connect() {
      if (stopped) return
      ws = new WebSocket(api.wsUrl())

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
    await api.killSession(id)
  }, [])

  const handleKillAll = useCallback(async () => {
    trackEvent('session_monitor_kill_all', { count: sessions.length })
    for (const s of sessions) {
      await api.killSession(s.session_id)
    }
  }, [sessions])

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
              <span className="font-semibold text-base">{strings.app.title}</span>
              <span className="opacity-30">|</span>
              <span className="text-sm font-medium opacity-80">
                {strings.app.subtitle}
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
                  {connected ? strings.app.connected : strings.app.disconnected}
                </span>
              </div>
              <span className="opacity-20">|</span>
              <Button
                variant="ghost"
                size="icon"
                onClick={() => setIsDarkMode(!isDarkMode)}
                title={isDarkMode ? strings.app.themeLight : strings.app.themeDark}
                className="h-7 w-7 opacity-60 hover:opacity-100"
              >
                {isDarkMode ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
              </Button>
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
          onKillAll={handleKillAll}
        />

        <div className="flex-1 overflow-hidden">
          {selected ? (
            <SessionDetail session={selected} onKill={() => handleKill(selected.session_id)} />
          ) : (
            <div className="flex flex-col items-center justify-center h-full text-muted-foreground gap-3">
              <Activity className="h-8 w-8 opacity-20" />
              <p className="text-sm font-medium">{strings.app.emptyTitle}</p>
              <p className="text-xs opacity-60 max-w-xs text-center">
                {strings.app.emptyBody}
              </p>
            </div>
          )}
        </div>
      </div>

      {/* Status bar */}
      <StatusBar wsConnected={connected} session={selected} />
    </div>
  )
}
