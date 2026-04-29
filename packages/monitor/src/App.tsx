import { useState, useEffect, useCallback, useMemo } from 'react'
import type {
  OperatorSessionSummary,
  OperatorSessionsResponse,
  OperatorWatchStatus,
  SessionInfo,
  WsMessage,
} from './types'
import SessionSidebar from './components/SessionSidebar'
import SessionDetail from './components/SessionDetail'
import StatusBar from './components/StatusBar'
import AppHeader from './components/AppHeader'
import EmptySessionState from './components/EmptySessionState'
import FunnelHero from './components/FunnelHero'
import ConnectOperatorModal from './components/ConnectOperatorModal'
import OperatorSessionDetail from './components/OperatorSessionDetail'
import { initAnalytics, setTelemetryEnabled, trackEvent } from './analytics'
import { api } from './api'
import { useTelemetryPref } from './hooks/useTelemetryPref'

const WS_RECONNECT_INTERVAL = 3000

type SidebarTab = 'mine' | 'team'

export default function App() {
  const [sessions, setSessions] = useState<SessionInfo[]>([])
  const [operatorSessions, setOperatorSessions] = useState<OperatorSessionSummary[]>([])
  const [watchStatus, setWatchStatus] = useState<OperatorWatchStatus | null>(null)
  const [selectedKind, setSelectedKind] = useState<'local' | 'operator' | null>(null)
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [connectModalOpen, setConnectModalOpen] = useState(false)
  const [activeTab, setActiveTab] = useState<SidebarTab>('mine')
  const [connected, setConnected] = useState(false)
  const [loading, setLoading] = useState(true)
  const [isDarkMode, setIsDarkMode] = useState(() => {
    const saved = localStorage.getItem('session-monitor-theme')
    if (saved) return saved === 'dark'
    return window.matchMedia('(prefers-color-scheme: dark)').matches
  })
  const [telemetryPref, setTelemetryPref] = useTelemetryPref()

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

  const refreshOperatorSessions = useCallback(() => {
    api.listOperatorSessions()
      .then((resp: OperatorSessionsResponse) => {
        setOperatorSessions(resp.sessions)
        setWatchStatus(resp.watch_status)
      })
      .catch((err) => {
        console.error(err)
        setWatchStatus({ status: 'unavailable', reason: String(err) })
      })
  }, [])

  useEffect(() => {
    refreshOperatorSessions()
    const t = setInterval(refreshOperatorSessions, 5000)
    return () => clearInterval(t)
  }, [refreshOperatorSessions])

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
        if (!stopped) reconnectTimer = setTimeout(connect, WS_RECONNECT_INTERVAL)
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
          setSessions((prev) =>
            prev.find((s) => s.session_id === session.session_id) ? prev : [...prev, session]
          )
        } else if (msg.type === 'session_removed') {
          const removedId = msg.session_id
          setSessions((prev) => prev.filter((s) => s.session_id !== removedId))
          setSelectedId((prev) => (prev === removedId && selectedKind === 'local' ? null : prev))
        } else if (msg.type === 'operator_session_added' || msg.type === 'operator_session_updated') {
          const session = msg.session
          setOperatorSessions((prev) => {
            const others = prev.filter((s) => s.id !== session.id)
            return [...others, session]
          })
        } else if (msg.type === 'operator_session_removed') {
          const removedId = msg.id
          setOperatorSessions((prev) => prev.filter((s) => s.id !== removedId))
          setSelectedId((prev) => (prev === removedId && selectedKind === 'operator' ? null : prev))
        }
      }
    }

    connect()
    return () => {
      stopped = true
      if (reconnectTimer) clearTimeout(reconnectTimer)
      ws?.close()
    }
  }, [selectedKind])

  const handleKill = useCallback(async (id: string) => {
    await api.killSession(id)
  }, [])

  const handleKillAll = useCallback(async () => {
    trackEvent('session_monitor_kill_all', { count: sessions.length })
    for (const s of sessions) {
      await api.killSession(s.session_id)
    }
  }, [sessions])

  const handleSelectLocal = useCallback((id: string) => {
    if (id === '') {
      setSelectedId(null)
      setSelectedKind(null)
      return
    }
    setSelectedId(id)
    setSelectedKind('local')
  }, [])

  const handleSelectOperator = useCallback((id: string) => {
    setSelectedId(id)
    setSelectedKind('operator')
  }, [])

  const selectedLocal = useMemo(
    () => (selectedKind === 'local' ? sessions.find((s) => s.session_id === selectedId) : undefined),
    [selectedKind, selectedId, sessions]
  )
  const selectedOperator = useMemo(
    () =>
      selectedKind === 'operator'
        ? operatorSessions.find((s) => s.id === selectedId)
        : undefined,
    [selectedKind, selectedId, operatorSessions]
  )

  const showFunnelHero =
    activeTab === 'team' &&
    !selectedOperator &&
    watchStatus?.status === 'unavailable'

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
          selectedId={selectedKind === 'local' ? selectedId : null}
          loading={loading}
          onSelect={handleSelectLocal}
          onKill={handleKill}
          onKillAll={handleKillAll}
          operatorSessions={operatorSessions}
          watchStatus={watchStatus}
          selectedOperatorId={selectedKind === 'operator' ? selectedId : null}
          onSelectOperator={handleSelectOperator}
          onConnectOperator={() => setConnectModalOpen(true)}
          activeTab={activeTab}
          onActiveTabChange={setActiveTab}
        />
        <div className="flex-1 overflow-hidden">
          {selectedLocal ? (
            <SessionDetail
              session={selectedLocal}
              onKill={() => handleKill(selectedLocal.session_id)}
            />
          ) : selectedOperator ? (
            <OperatorSessionDetail session={selectedOperator} />
          ) : showFunnelHero ? (
            <FunnelHero onConnect={() => setConnectModalOpen(true)} />
          ) : (
            <EmptySessionState />
          )}
        </div>
      </div>
      <StatusBar wsConnected={connected} session={selectedLocal} />
      <ConnectOperatorModal
        open={connectModalOpen}
        onOpenChange={setConnectModalOpen}
        watchStatus={watchStatus}
      />
    </div>
  )
}
