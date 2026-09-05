import type {
  ChaosRule,
  ChaosRuleRequest,
  ContextsResponse,
  NamespacesResponse,
  OperatorLicense,
  OperatorSessionsResponse,
  SessionInfo,
} from './types'
import { emitUserBlocked, emitUserSucceeded } from './analytics'
import { ApiError } from './apiError'

const HTTP_NOT_FOUND = 404

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

function chaosRulesPath(sessionId: string, ruleId?: string): string {
  const base = `/api/chaos/rules/${encodeURIComponent(sessionId)}`
  return ruleId ? `${base}/${encodeURIComponent(ruleId)}` : base
}

async function chaosErrorMessage(r: Response): Promise<string> {
  const body = await r.text().catch(() => '')
  return body || `${r.status} ${r.statusText}`
}

export const api = {
  listSessions: async (): Promise<SessionInfo[]> => {
    const r = await fetch(withToken('/api/v2/local/sessions'), {
      credentials: 'include',
    })
    if (!r.ok) {
      throw new ApiError(
        r.status,
        `Failed to fetch sessions: ${r.status} ${r.statusText}`,
      )
    }
    const data = (await r.json()) as SessionInfo[]
    return data
  },

  getSession: async (sessionId: string): Promise<SessionInfo | null> => {
    const r = await fetch(
      withToken(`/api/v2/local/sessions/${encodeURIComponent(sessionId)}`),
      {
        credentials: 'include',
      },
    )
    if (!r.ok) {
      if (r.status !== HTTP_NOT_FOUND) {
        emitUserBlocked('session_fetch_failed', {
          session_id: sessionId,
          status: r.status,
          error: r.statusText,
        })
      }
      return null
    }
    emitUserSucceeded('session_loaded', {
      session_id: sessionId,
    })
    const data = (await r.json()) as SessionInfo
    return data
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
      emitUserBlocked(
        'session_kill_failed',
        {
          session_id: sessionId,
          error: err instanceof Error ? err.message : String(err),
        },
        err,
      )
      return
    }
    if (!r.ok) {
      emitUserBlocked('session_kill_failed', {
        session_id: sessionId,
        status: r.status,
        error: r.statusText,
      })
    } else {
      emitUserSucceeded('session_killed', {
        session_id: sessionId,
      })
    }
  },

  eventStreamUrl: (sessionId: string): string =>
    withToken(`/api/v2/local/sessions/${encodeURIComponent(sessionId)}/events`),

  listChaosRules: async (sessionId: string): Promise<ChaosRule[]> => {
    const r = await fetch(withToken(chaosRulesPath(sessionId)), {
      credentials: 'include',
    })
    if (!r.ok) {
      if (r.status === HTTP_NOT_FOUND) return []
      throw new ApiError(r.status, await chaosErrorMessage(r))
    }
    return (await r.json()) as ChaosRule[]
  },

  createChaosRule: async (
    sessionId: string,
    rule: ChaosRuleRequest,
  ): Promise<ChaosRule> => {
    const r = await fetch(withToken(chaosRulesPath(sessionId)), {
      method: 'POST',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(rule),
    })
    if (!r.ok) {
      emitUserBlocked('chaos_rule_create_failed', {
        session_id: sessionId,
        status: r.status,
      })
      throw new ApiError(r.status, await chaosErrorMessage(r))
    }
    emitUserSucceeded('chaos_rule_created', {
      session_id: sessionId,
    })
    return (await r.json()) as ChaosRule
  },

  updateChaosRule: async (
    sessionId: string,
    ruleId: string,
    rule: ChaosRuleRequest,
  ): Promise<ChaosRule> => {
    const r = await fetch(withToken(chaosRulesPath(sessionId, ruleId)), {
      method: 'PUT',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(rule),
    })
    if (!r.ok) {
      emitUserBlocked('chaos_rule_update_failed', {
        session_id: sessionId,
        rule_id: ruleId,
        status: r.status,
      })
      throw new ApiError(r.status, await chaosErrorMessage(r))
    }
    emitUserSucceeded('chaos_rule_updated', {
      session_id: sessionId,
      rule_id: ruleId,
    })
    return (await r.json()) as ChaosRule
  },

  deleteChaosRule: async (sessionId: string, ruleId: string): Promise<void> => {
    const r = await fetch(withToken(chaosRulesPath(sessionId, ruleId)), {
      method: 'DELETE',
      credentials: 'include',
    })
    if (!r.ok) {
      emitUserBlocked('chaos_rule_delete_failed', {
        session_id: sessionId,
        rule_id: ruleId,
        status: r.status,
      })
      throw new ApiError(r.status, await chaosErrorMessage(r))
    }
    emitUserSucceeded('chaos_rule_deleted', {
      session_id: sessionId,
      rule_id: ruleId,
    })
  },

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
    const r = await fetch(withToken(path), { credentials: 'include' })
    if (!r.ok) {
      throw new ApiError(
        r.status,
        `Failed to fetch operator sessions: ${r.status} ${r.statusText}`,
      )
    }
    const data = (await r.json()) as OperatorSessionsResponse
    return data
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
    const data = (await r.json()) as OperatorLicense
    return data
  },

  listContexts: async (): Promise<ContextsResponse> => {
    const r = await fetch(withToken('/api/v2/kube/contexts'), {
      credentials: 'include',
    })
    if (!r.ok)
      throw new ApiError(
        r.status,
        `Failed to fetch contexts: ${r.status} ${r.statusText}`,
      )
    const data = (await r.json()) as ContextsResponse
    return data
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
      throw new ApiError(
        r.status,
        `Failed to fetch namespaces: ${r.status} ${r.statusText}`,
      )
    const data = (await r.json()) as NamespacesResponse
    return data
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
      emitUserBlocked('me_fetch_failed', {
        status: r.status,
        error: r.statusText,
      })
      return { k8sUsername: null }
    }
    const data = (await r.json()) as { username?: string | null }
    emitUserSucceeded('me_loaded')
    return { k8sUsername: data.username ?? null }
  },
}
