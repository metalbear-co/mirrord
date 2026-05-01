import { useState, useEffect, useCallback, useMemo, useRef } from 'react'
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
import AppHeader, { type HeaderActivity } from './components/AppHeader'
import { formatUptime } from './utils'
import EmptySessionState from './components/EmptySessionState'
import FunnelHero from './components/FunnelHero'
import ConnectOperatorModal from './components/ConnectOperatorModal'
import OperatorSessionDetail from './components/OperatorSessionDetail'
import { applyDark, loadTheme, resolveDark, saveTheme, type ThemePref } from './theme'
import { initAnalytics, setTelemetryEnabled, trackEvent } from './analytics'
import { api } from './api'
import { useTelemetryPref } from './hooks/useTelemetryPref'
import {
  pingExtension,
  joinViaExtension,
  leaveViaExtension,
  type ExtensionState,
} from './extensionBridge'

const WS_RECONNECT_INTERVAL = 3000

export default function App() {
  const [sessions, setSessions] = useState<SessionInfo[]>([])
  const [operatorSessions, setOperatorSessions] = useState<OperatorSessionSummary[]>([])
  const [watchStatus, setWatchStatus] = useState<OperatorWatchStatus | null>(null)
  const [selectedKind, setSelectedKind] = useState<'local' | 'operator' | null>(null)
  const selectedKindRef = useRef(selectedKind)
  useEffect(() => {
    selectedKindRef.current = selectedKind
  }, [selectedKind])
  const [selectedId, setSelectedId] = useState<string | null>(null)
  const [connectModalOpen, setConnectModalOpen] = useState(false)
  const [extensionState, setExtensionState] = useState<ExtensionState>({
    installed: false,
    supportsBridge: false,
  })
  const [connected, setConnected] = useState(false)
  const [loading, setLoading] = useState(true)
  const [theme, setTheme] = useState<ThemePref>(() => loadTheme())
  const isDarkMode = resolveDark(theme)
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
    applyDark(isDarkMode)
    saveTheme(theme)
  }, [theme, isDarkMode])

  useEffect(() => {
    if (theme !== 'system') return
    const media = window.matchMedia('(prefers-color-scheme: dark)')
    const handler = () => applyDark(media.matches)
    media.addEventListener('change', handler)
    return () => media.removeEventListener('change', handler)
  }, [theme])

  useEffect(() => {
    if (selectedKind && selectedId) return
    if (sessions.length > 0) {
      setSelectedKind('local')
      setSelectedId(sessions[0].session_id)
      return
    }
    if (operatorSessions.length > 0) {
      setSelectedKind('operator')
      setSelectedId(operatorSessions[0].id)
    }
  }, [sessions, operatorSessions, selectedKind, selectedId])

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

  const refreshExtensionState = useCallback(async () => {
    const state = await pingExtension()
    setExtensionState(state)
  }, [])

  useEffect(() => {
    refreshExtensionState()
    const t = setInterval(refreshExtensionState, 4000)
    return () => clearInterval(t)
  }, [refreshExtensionState])

  const handleJoinViaExtension = useCallback(
    async (key: string) => {
      const result = await joinViaExtension(key)
      if (result.ok) {
        setExtensionState((prev) => ({ ...prev, joinedKey: result.joinedKey ?? key }))
      }
      return result
    },
    []
  )

  const handleLeaveViaExtension = useCallback(async () => {
    const result = await leaveViaExtension()
    if (result.ok) {
      setExtensionState((prev) => ({ ...prev, joinedKey: null }))
    }
    return result
  }, [])

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
          setSelectedId((prev) =>
            prev === removedId && selectedKindRef.current === 'local' ? null : prev
          )
        } else if (msg.type === 'operator_session_added' || msg.type === 'operator_session_updated') {
          const session = msg.session
          setOperatorSessions((prev) => {
            const idx = prev.findIndex((s) => s.id === session.id)
            if (idx === -1) return [...prev, session]
            const next = prev.slice()
            next[idx] = session
            return next
          })
        } else if (msg.type === 'operator_session_removed') {
          const removedId = msg.id
          setOperatorSessions((prev) => prev.filter((s) => s.id !== removedId))
          setSelectedId((prev) =>
            prev === removedId && selectedKindRef.current === 'operator'
              ? null
              : prev
          )
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

  const localIds = useMemo(() => new Set(sessions.map((s) => s.session_id)), [sessions])
  const [currentUserK8s, setCurrentUserK8s] = useState<string | null>(null)
  useEffect(() => {
    let cancelled = false
    api
      .currentUser()
      .then(({ k8sUsername }) => {
        if (!cancelled) setCurrentUserK8s(k8sUsername)
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [])
  const yoursOperatorSessions = useMemo(
    () =>
      currentUserK8s
        ? operatorSessions.filter(
            (s) =>
              !localIds.has(s.id) && s.owner.k8sUsername === currentUserK8s
          )
        : [],
    [operatorSessions, localIds, currentUserK8s]
  )
  const teamSessions = useMemo(
    () =>
      operatorSessions.filter(
        (s) =>
          !localIds.has(s.id) &&
          (!currentUserK8s || s.owner.k8sUsername !== currentUserK8s)
      ),
    [operatorSessions, localIds, currentUserK8s]
  )

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
    !selectedLocal &&
    !selectedOperator &&
    watchStatus?.status === 'unavailable'

  const [searchQuery, setSearchQuery] = useState('')

  const headerActivity = useMemo<HeaderActivity | null>(() => {
    const joinedKey = extensionState.joinedKey ?? null
    if (selectedLocal) {
      const isJoined = !!joinedKey && selectedLocal.key === joinedKey
      return {
        status: isJoined ? 'Joined' : 'Watching',
        target: selectedLocal.target,
        uptime: formatUptime(selectedLocal.started_at),
      }
    }
    if (selectedOperator) {
      const isJoined = !!joinedKey && selectedOperator.key === joinedKey
      const target = selectedOperator.target
        ? `${selectedOperator.target.kind}/${selectedOperator.target.name}`
        : 'targetless'
      return {
        status: isJoined ? 'Joined' : 'Watching',
        target,
        uptime: formatUptime(selectedOperator.createdAt),
      }
    }
    return null
  }, [selectedLocal, selectedOperator, extensionState.joinedKey])

  return (
    <div className="h-screen flex flex-col bg-background text-foreground">
      <AppHeader
        connected={connected}
        isDarkMode={isDarkMode}
        onToggleTheme={() => setTheme(isDarkMode ? 'light' : 'dark')}
        theme={theme}
        onThemeChange={setTheme}
        telemetryEnabled={telemetryPref}
        onTelemetryChange={setTelemetryPref}
        activity={headerActivity}
        query={searchQuery}
        onQueryChange={setSearchQuery}
      />
      <div className="flex flex-1 overflow-hidden">
        <SessionSidebar
          sessions={sessions}
          selectedId={selectedKind === 'local' ? selectedId : null}
          loading={loading}
          onSelect={handleSelectLocal}
          onKill={handleKill}
          onKillAll={handleKillAll}
          operatorSessions={teamSessions}
          yoursOperatorSessions={yoursOperatorSessions}
          allOperatorSessions={operatorSessions}
          watchStatus={watchStatus}
          selectedOperatorId={selectedKind === 'operator' ? selectedId : null}
          onSelectOperator={handleSelectOperator}
          onConnectOperator={() => setConnectModalOpen(true)}
          joinedKey={extensionState.joinedKey ?? null}
          query={searchQuery}
        />
        <div className="flex-1 overflow-hidden">
          {selectedLocal ? (
            <SessionDetail
              session={selectedLocal}
              onKill={() => handleKill(selectedLocal.session_id)}
              extensionState={extensionState}
              onJoin={() => handleJoinViaExtension(selectedLocal.key ?? '')}
              onLeave={handleLeaveViaExtension}
            />
          ) : selectedOperator ? (
            <OperatorSessionDetail
              session={selectedOperator}
              extensionState={extensionState}
              onJoin={() => handleJoinViaExtension(selectedOperator.key)}
              onLeave={handleLeaveViaExtension}
            />
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
