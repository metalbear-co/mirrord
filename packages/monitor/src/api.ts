import type { OperatorSessionsResponse, SessionInfo } from './types'
import { emitUserBlocked, emitUserSucceeded } from './analytics'

let authToken: string | null = null

if (typeof window !== 'undefined') {
  const urlToken = new URLSearchParams(window.location.search).get('token')
  if (urlToken) {
    authToken = urlToken
    sessionStorage.setItem('mirrord_ui_token', urlToken)
  } else {
    authToken = sessionStorage.getItem('mirrord_ui_token')
  }
}

function withToken(path: string): string {
  if (!authToken) return path
  const sep = path.includes('?') ? '&' : '?'
  return `${path}${sep}token=${encodeURIComponent(authToken)}`
}

let sessionsHealthy = true
let operatorSessionsHealthy = true

function reportSessionsHealth(
  endpoint: 'sessions' | 'operator_sessions',
  healthy: boolean,
  error?: string,
  status?: number,
): void {
  const currentlyHealthy =
    endpoint === 'sessions' ? sessionsHealthy : operatorSessionsHealthy
  if (healthy && !currentlyHealthy) {
    if (endpoint === 'sessions') sessionsHealthy = true
    else operatorSessionsHealthy = true
    emitUserSucceeded('sessions_healthy', 'health', { endpoint })
  } else if (!healthy && currentlyHealthy) {
    if (endpoint === 'sessions') sessionsHealthy = false
    else operatorSessionsHealthy = false
    emitUserBlocked('sessions_unhealthy', 'health', {
      endpoint,
      ...(error !== undefined && { error }),
      ...(status !== undefined && { status }),
    })
  }
}

export const api = {
  listSessions: async (): Promise<SessionInfo[]> => {
    try {
      const r = await fetch(withToken('/api/sessions'), { credentials: 'include' })
      if (!r.ok) {
        reportSessionsHealth('sessions', false, r.statusText, r.status)
        throw new Error(`Failed to fetch sessions: ${r.status} ${r.statusText}`)
      }
      reportSessionsHealth('sessions', true)
      return r.json()
    } catch (err) {
      if (!(err instanceof Error) || !err.message.startsWith('Failed to fetch sessions')) {
        const error = err instanceof Error ? err.message : String(err)
        reportSessionsHealth('sessions', false, error)
      }
      throw err
    }
  },

  getSession: async (sessionId: string): Promise<SessionInfo | null> => {
    const r = await fetch(withToken(`/api/sessions/${encodeURIComponent(sessionId)}`), {
      credentials: 'include',
    })
    if (!r.ok) {
      if (r.status !== 404) {
        emitUserBlocked('session_fetch_failed', 'user_action', {
          session_id: sessionId,
          status: r.status,
          error: r.statusText,
        })
      }
      return null
    }
    emitUserSucceeded('session_loaded', 'user_action', { session_id: sessionId })
    return r.json()
  },

  killSession: async (sessionId: string): Promise<Response> => {
    const r = await fetch(withToken(`/api/sessions/${encodeURIComponent(sessionId)}/kill`), {
      method: 'POST',
      credentials: 'include',
    })
    if (!r.ok) {
      emitUserBlocked('session_kill_failed', 'user_action', {
        session_id: sessionId,
        status: r.status,
        error: r.statusText,
      })
    } else {
      emitUserSucceeded('session_killed', 'user_action', { session_id: sessionId })
    }
    return r
  },

  eventStreamUrl: (sessionId: string): string =>
    withToken(`/api/sessions/${encodeURIComponent(sessionId)}/events`),

  listOperatorSessions: async (): Promise<OperatorSessionsResponse> => {
    try {
      const r = await fetch(withToken('/api/operator-sessions'), {
        credentials: 'include',
      })
      if (!r.ok) {
        reportSessionsHealth('operator_sessions', false, r.statusText, r.status)
        throw new Error(
          `Failed to fetch operator sessions: ${r.status} ${r.statusText}`
        )
      }
      reportSessionsHealth('operator_sessions', true)
      return r.json()
    } catch (err) {
      if (
        !(err instanceof Error) ||
        !err.message.startsWith('Failed to fetch operator sessions')
      ) {
        const error = err instanceof Error ? err.message : String(err)
        reportSessionsHealth('operator_sessions', false, error)
      }
      throw err
    }
  },

  currentUser: async (): Promise<{ k8sUsername: string | null }> => {
    const r = await fetch(withToken('/api/me'), { credentials: 'include' })
    if (!r.ok) {
      emitUserBlocked('me_fetch_failed', 'user_action', {
        status: r.status,
        error: r.statusText,
      })
      return { k8sUsername: null }
    }
    const data = (await r.json()) as { k8sUsername?: string | null }
    emitUserSucceeded('me_loaded', 'user_action')
    return { k8sUsername: data.k8sUsername ?? null }
  },

  wsUrl: (): string => {
    const scheme = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
    const base = `${scheme}//${window.location.host}/ws`
    return authToken ? `${base}?token=${encodeURIComponent(authToken)}` : base
  },
}
