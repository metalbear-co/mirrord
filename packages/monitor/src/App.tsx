import { useState, useEffect, useCallback, useMemo, useRef } from 'react'
import type {
  KubeContext,
  OperatorSessionSummary,
  OperatorWatchStatus,
  SessionInfo,
} from './types'
import SessionSidebar from './components/SessionSidebar'
import SessionDetail from './components/SessionDetail'
import AppHeader from './components/AppHeader'
import EmptySessionState from './components/EmptySessionState'
import FunnelHero from './components/FunnelHero'
import ConnectOperatorModal from './components/ConnectOperatorModal'
import OperatorSessionDetail from './components/OperatorSessionDetail'
import type { ThemePref } from './theme'
import {
  initAnalytics,
  setTelemetryEnabled,
  setLicenseGroup,
  trackEvent,
  emitUserBlocked,
  emitUserSucceeded,
} from './analytics'
import { api } from './api'
import { useTelemetryPref } from './hooks/useTelemetryPref'
import {
  pingExtension,
  joinViaExtension,
  leaveViaExtension,
  type ExtensionState,
} from './extensionBridge'

const LOCAL_POLL_INTERVAL = 2000

/**
 * Theme is owned by the `mirrord-ui` shell (so a single top-right toggle controls both tabs). The
 * monitor receives the resolved values and forwards them to its settings dialog and header logo.
 */
export type MonitorProps = {
  theme: ThemePref
  isDarkMode: boolean
  onThemeChange: (theme: ThemePref) => void
  // Whether the monitor tab is the one on screen. The monitor stays mounted while hidden, so
  // its top-bar controls only portal into the shell when it is actually active. Defaults to
  // true so a standalone mount still shows them.
  active?: boolean
}

export default function App({
  theme,
  isDarkMode,
  onThemeChange,
  active = true,
}: MonitorProps) {
  const [sessions, setSessions] = useState<SessionInfo[]>([])
  const [operatorSessions, setOperatorSessions] = useState<
    OperatorSessionSummary[]
  >([])
  const [watchStatus, setWatchStatus] = useState<OperatorWatchStatus | null>(
    null,
  )
  const [selectedKind, setSelectedKind] = useState<'local' | 'operator' | null>(
    null,
  )
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
  const [telemetryPref, setTelemetryPref] = useTelemetryPref()

  // Context/namespace selection. `selectedContext === null` means "follow the kubeconfig's current
  // context". Both are per-tab React state, so two tabs view two clusters independently — the server
  // holds no shared "current context".
  const [contexts, setContexts] = useState<KubeContext[]>([])
  const [currentContext, setCurrentContext] = useState<string | null>(null)
  const [selectedContext, setSelectedContext] = useState<string | null>(null)
  const [namespaces, setNamespaces] = useState<string[]>([])
  const [namespacesLoading, setNamespacesLoading] = useState(false)
  // Set when listing namespaces fails — e.g. an RBAC policy that denies `list namespaces` (common on
  // strict multi-tenant clusters). The picker then lets the user type a namespace by hand instead.
  const [namespacesError, setNamespacesError] = useState(false)
  // `null` = all namespaces. Applied server-side (the operator-sessions request carries it); local
  // sessions are never filtered by it.
  const [selectedNamespace, setSelectedNamespace] = useState<string | null>(
    null,
  )
  const effectiveContext = selectedContext ?? currentContext

  const defaultNamespaceFor = useCallback(
    (context: string | null): string | null =>
      context
        ? (contexts.find((c) => c.name === context)?.namespace ?? null)
        : null,
    [contexts],
  )

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
      (s) => (s.config as Record<string, unknown>)?.telemetry !== false,
    )
    const shouldCapture = sessionAllowsTelemetry && telemetryPref
    initAnalytics(shouldCapture)
    setTelemetryEnabled(shouldCapture)
  }, [sessions, telemetryPref])

  // Local sessions are host-global and change on user action; poll them (the "connected" indicator
  // reflects poll health). No WebSocket: every v2 resource is fetched over HTTP.
  useEffect(() => {
    let cancelled = false
    const poll = () => {
      api
        .listSessions()
        .then((data) => {
          if (cancelled) return
          setSessions(data)
          setConnected(true)
        })
        .catch((err) => {
          if (cancelled) return
          console.error(err)
          setConnected(false)
        })
        .finally(() => {
          if (!cancelled) setLoading(false)
        })
    }
    poll()
    const t = setInterval(poll, LOCAL_POLL_INTERVAL)
    return () => {
      cancelled = true
      clearInterval(t)
    }
  }, [])

  // Clear a stale local selection when its session disappears from the polled list.
  useEffect(() => {
    if (selectedKind !== 'local' || selectedId == null) return
    if (!sessions.some((s) => s.session_id === selectedId)) setSelectedId(null)
  }, [sessions, selectedId, selectedKind])

  // Load the kube contexts once, and default the namespace filter to the current context's
  // configured namespace (shipped inline on each context) so the first view matches a plain
  // `mirrord exec`.
  useEffect(() => {
    api
      .listContexts()
      .then(({ current, contexts }) => {
        setContexts(contexts)
        setCurrentContext(current)
        setSelectedNamespace(
          contexts.find((c) => c.name === current)?.namespace ?? null,
        )
      })
      .catch((err) => console.error(err))
  }, [])

  // Populate the namespace dropdown for the active context. Listing can be denied by RBAC, in which
  // case we flag the error so the picker offers free-text entry instead.
  useEffect(() => {
    if (!effectiveContext) return
    let cancelled = false
    setNamespacesLoading(true)
    api
      .listNamespaces(effectiveContext)
      .then(({ namespaces }) => {
        if (cancelled) return
        setNamespaces(namespaces)
        setNamespacesError(false)
      })
      .catch((err) => {
        if (cancelled) return
        console.error(err)
        setNamespaces([])
        setNamespacesError(true)
      })
      .finally(() => {
        if (!cancelled) setNamespacesLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [effectiveContext])

  // The operator license identifies the licensed organization; fetch it once per context so ui
  // usage is attributed to that org in analytics, rather than on every session poll.
  useEffect(() => {
    let cancelled = false
    api
      .getOperatorLicense(effectiveContext)
      .then((license) => {
        if (cancelled || !license?.fingerprint) return
        setLicenseGroup(license.fingerprint, license.organization)
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [effectiveContext])

  const refreshOperatorSessions = useCallback(() => {
    api
      .listOperatorSessions(effectiveContext, selectedNamespace)
      .then((resp) => {
        setOperatorSessions(resp.sessions)
        setWatchStatus(
          resp.status === 'available'
            ? { status: 'watching' }
            : {
                status: 'unavailable',
                reason: resp.reason ?? 'operator not available',
              },
        )
      })
      .catch((err) => {
        console.error(err)
        setWatchStatus({ status: 'unavailable', reason: String(err) })
      })
  }, [effectiveContext, selectedNamespace])

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

  const handleJoinViaExtension = useCallback(async (key: string) => {
    const result = await joinViaExtension(key)
    if (result.ok) {
      setExtensionState((prev) => ({
        ...prev,
        joinedKey: result.joinedKey ?? key,
      }))
      emitUserSucceeded('operator_session_joined', 'user_action', { key })
    } else {
      emitUserBlocked('operator_session_join_failed', 'user_action', {
        key,
        ...(result.error && { error: result.error }),
      })
    }
    return result
  }, [])

  const handleLeaveViaExtension = useCallback(async () => {
    const result = await leaveViaExtension()
    if (result.ok) {
      setExtensionState((prev) => ({ ...prev, joinedKey: null }))
    }
    return result
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

  const handleSelectContext = useCallback(
    (context: string | null) => {
      setSelectedContext(context)
      setSelectedNamespace(defaultNamespaceFor(context))
      // Drop the previous cluster's sessions immediately; the poll refetches for the new context.
      setOperatorSessions([])
      setSelectedId((prev) =>
        selectedKindRef.current === 'operator' ? null : prev,
      )
    },
    [defaultNamespaceFor],
  )

  const localIds = useMemo(
    () => new Set(sessions.map((s) => s.session_id)),
    [sessions],
  )
  const [currentUserK8s, setCurrentUserK8s] = useState<string | null>(null)
  useEffect(() => {
    let cancelled = false
    api
      .currentUser(effectiveContext)
      .then(({ k8sUsername }) => {
        if (!cancelled) setCurrentUserK8s(k8sUsername)
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [effectiveContext])
  const yoursOperatorSessions = useMemo(
    () =>
      currentUserK8s
        ? operatorSessions.filter(
            (s) =>
              !localIds.has(s.id) && s.owner.k8sUsername === currentUserK8s,
          )
        : [],
    [operatorSessions, localIds, currentUserK8s],
  )
  const teamSessions = useMemo(
    () =>
      operatorSessions.filter(
        (s) =>
          !localIds.has(s.id) &&
          (!currentUserK8s || s.owner.k8sUsername !== currentUserK8s),
      ),
    [operatorSessions, localIds, currentUserK8s],
  )

  const selectedLocal = useMemo(
    () =>
      selectedKind === 'local'
        ? sessions.find((s) => s.session_id === selectedId)
        : undefined,
    [selectedKind, selectedId, sessions],
  )
  const selectedOperator = useMemo(
    () =>
      selectedKind === 'operator'
        ? operatorSessions.find((s) => s.id === selectedId)
        : undefined,
    [selectedKind, selectedId, operatorSessions],
  )

  const showFunnelHero =
    !selectedLocal && !selectedOperator && watchStatus?.status === 'unavailable'

  const [searchQuery, setSearchQuery] = useState('')

  return (
    <div className="h-full flex flex-col bg-background text-foreground">
      <AppHeader
        active={active}
        connected={connected}
        isDarkMode={isDarkMode}
        theme={theme}
        onThemeChange={onThemeChange}
        telemetryEnabled={telemetryPref}
        onTelemetryChange={setTelemetryPref}
        currentUser={currentUserK8s}
        contexts={contexts}
        currentContext={currentContext}
        selectedContext={selectedContext}
        onSelectContext={handleSelectContext}
        namespaces={namespaces}
        selectedNamespace={selectedNamespace}
        onSelectNamespace={setSelectedNamespace}
        namespacesLoading={namespacesLoading}
        namespacesError={namespacesError}
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
          onQueryChange={setSearchQuery}
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
      <ConnectOperatorModal
        open={connectModalOpen}
        onOpenChange={setConnectModalOpen}
        watchStatus={watchStatus}
      />
    </div>
  )
}
