import { useState, useEffect, useCallback } from 'react'
import type { SessionInfo, WsMessage } from './types'
import SessionSidebar from './components/SessionSidebar'
import SessionDetail from './components/SessionDetail'
import StatusBar from './components/StatusBar'
import AppHeader from './components/AppHeader'
import EmptySessionState from './components/EmptySessionState'
import { initAnalytics, setTelemetryEnabled, trackEvent } from './analytics'
import { api } from './api'
import { useTelemetryPref } from './hooks/useTelemetryPref'

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
  const [telemetryPref, setTelemetryPref] = useTelemetryPref()

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

  // Init PostHog once a session exists and both the session's own `config.telemetry` and
  // the user's local preference allow it. After init, `setTelemetryEnabled` flips opt-in
  // at runtime so toggling from Settings takes effect without a reload.
  useEffect(() => {
    if (sessions.length === 0) return
    const sessionAllowsTelemetry = sessions.every(
      s => (s.config as Record<string, unknown>)?.telemetry !== false
    )
    const shouldCapture = sessionAllowsTelemetry && telemetryPref
    initAnalytics(shouldCapture)
    setTelemetryEnabled(shouldCapture)
  }, [sessions, telemetryPref])

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
      <AppHeader
        connected={connected}
        isDarkMode={isDarkMode}
        onToggleTheme={() => setIsDarkMode(!isDarkMode)}
        telemetryEnabled={telemetryPref}
        onTelemetryChange={setTelemetryPref}
      />
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
            <EmptySessionState />
          )}
        </div>
      </div>
      <StatusBar wsConnected={connected} session={selected} />
    </div>
  )
}
