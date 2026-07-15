import type {
  ContextsResponse,
  NamespacesResponse,
  OperatorLicense,
  OperatorSessionsResponse,
  SessionInfo,
} from './types'
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

// The context is a query param on every cluster-touching endpoint (null = kubeconfig current
// context), so each browser tab drives its own selection independently.
function contextParam(context: string | null): string {
  return context ? `?context=${encodeURIComponent(context)}` : ''
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
      const r = await fetch(withToken('/api/v2/local/sessions'), {
        credentials: 'include',
      })
      if (!r.ok) {
        reportSessionsHealth('sessions', false, r.statusText, r.status)
        throw new Error(`Failed to fetch sessions: ${r.status} ${r.statusText}`)
      }
      reportSessionsHealth('sessions', true)
      return r.json()
    } catch (err) {
      if (
        !(err instanceof Error) ||
        !err.message.startsWith('Failed to fetch sessions')
      ) {
        const error = err instanceof Error ? err.message : String(err)
        reportSessionsHealth('sessions', false, error)
      }
      throw err
    }
  },

  getSession: async (sessionId: string): Promise<SessionInfo | null> => {
    const r = await fetch(
      withToken(`/api/v2/local/sessions/${encodeURIComponent(sessionId)}`),
      {
        credentials: 'include',
      },
    )
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
    emitUserSucceeded('session_loaded', 'user_action', {
      session_id: sessionId,
    })
    return r.json()
  },

  killSession: async (sessionId: string): Promise<void> => {
    let r: Response
    try {
      r = await fetch(
        withToken(`/api/v2/local/sessions/${encodeURIComponent(sessionId)}`),
        {
          method: 'DELETE',
          credentials: 'include',
        },
      )
    } catch (err) {
      emitUserBlocked('session_kill_failed', 'user_action', {
        session_id: sessionId,
        error: err instanceof Error ? err.message : String(err),
      })
      return
    }
    if (!r.ok) {
      emitUserBlocked('session_kill_failed', 'user_action', {
        session_id: sessionId,
        status: r.status,
        error: r.statusText,
      })
    } else {
      emitUserSucceeded('session_killed', 'user_action', {
        session_id: sessionId,
      })
    }
  },

  eventStreamUrl: (sessionId: string): string =>
    withToken(`/api/v2/local/sessions/${encodeURIComponent(sessionId)}/events`),

  // Cluster sessions for the selected context, filtered to the selected namespace (null = all).
  listOperatorSessions: async (
    context: string | null,
    namespace: string | null,
  ): Promise<OperatorSessionsResponse> => {
    const params = new URLSearchParams()
    if (context) params.set('context', context)
    if (namespace) params.set('namespace', namespace)
    const qs = params.toString()
    const path = qs
      ? `/api/v2/operator/sessions?${qs}`
      : '/api/v2/operator/sessions'
    try {
      const r = await fetch(withToken(path), { credentials: 'include' })
      if (!r.ok) {
        reportSessionsHealth('operator_sessions', false, r.statusText, r.status)
        throw new Error(
          `Failed to fetch operator sessions: ${r.status} ${r.statusText}`,
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

  getOperatorLicense: async (
    context: string | null,
  ): Promise<OperatorLicense | null> => {
    const r = await fetch(
      withToken(`/api/v2/operator/license${contextParam(context)}`),
      {
        credentials: 'include',
      },
    )
    if (!r.ok) return null
    return r.json()
  },

  listContexts: async (): Promise<ContextsResponse> => {
    const r = await fetch(withToken('/api/v2/kube/contexts'), {
      credentials: 'include',
    })
    if (!r.ok)
      throw new Error(`Failed to fetch contexts: ${r.status} ${r.statusText}`)
    return r.json()
  },

  listNamespaces: async (
    context: string | null,
  ): Promise<NamespacesResponse> => {
    const r = await fetch(
      withToken(`/api/v2/kube/namespaces${contextParam(context)}`),
      {
        credentials: 'include',
      },
    )
    if (!r.ok)
      throw new Error(`Failed to fetch namespaces: ${r.status} ${r.statusText}`)
    return r.json()
  },

  currentUser: async (
    context: string | null,
  ): Promise<{ k8sUsername: string | null }> => {
    const r = await fetch(
      withToken(`/api/v2/kube/user${contextParam(context)}`),
      {
        credentials: 'include',
      },
    )
    if (!r.ok) {
      emitUserBlocked('me_fetch_failed', 'user_action', {
        status: r.status,
        error: r.statusText,
      })
      return { k8sUsername: null }
    }
    const data = (await r.json()) as { username?: string | null }
    emitUserSucceeded('me_loaded', 'user_action')
    return { k8sUsername: data.username ?? null }
  },
}
