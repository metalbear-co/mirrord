import type {
  ChaosRule,
  ChaosRuleRequest,
  ContextsResponse,
  NamespacesResponse,
  OperatorLicense,
  OperatorSessionsResponse,
  PreviewDetail,
  SessionInfo,
} from './types'
import { emitUserBlocked, emitUserSucceeded } from './analytics'

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

/**
 * A failed `api` call, tagged with the endpoint it came from.
 *
 * `status` is absent when the request never reached a response — a rejected `fetch` carries
 * only the browser's generic message (`Failed to fetch`), which is identical for a stopped
 * daemon, a refused port and a blocked request, and names no endpoint at all.
 */
export class ApiError extends Error {
  readonly endpoint: string
  readonly status: number | undefined

  constructor(endpoint: string, message: string, status?: number) {
    super(message)
    this.name = 'ApiError'
    this.endpoint = endpoint
    this.status = status
  }
}

/** Telemetry properties describing which request failed, for `emitUserBlocked` call sites. */
export function requestContext(err: unknown): {
  endpoint?: string
  status?: number
} {
  if (!(err instanceof ApiError)) return {}
  return err.status === undefined
    ? { endpoint: err.endpoint }
    : { endpoint: err.endpoint, status: err.status }
}

async function request(
  endpoint: string,
  path: string,
  init?: RequestInit,
): Promise<Response> {
  try {
    return await fetch(withToken(path), { credentials: 'include', ...init })
  } catch (err) {
    throw new ApiError(
      endpoint,
      err instanceof Error ? err.message : String(err),
    )
  }
}

export const api = {
  listSessions: async (): Promise<SessionInfo[]> => {
    const r = await request('sessions', '/api/v2/local/sessions')
    if (!r.ok) {
      throw new ApiError(
        'sessions',
        `Failed to fetch sessions: ${r.status} ${r.statusText}`,
        r.status,
      )
    }
    const data = (await r.json()) as SessionInfo[]
    return data
  },

  getSession: async (sessionId: string): Promise<SessionInfo | null> => {
    const r = await request(
      'session',
      `/api/v2/local/sessions/${encodeURIComponent(sessionId)}`,
    )
    if (!r.ok) {
      if (r.status !== HTTP_NOT_FOUND) {
        emitUserBlocked('session_fetch_failed', {
          session_id: sessionId,
          endpoint: 'session',
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
      r = await request(
        'session_kill',
        `/api/v2/local/sessions/${encodeURIComponent(sessionId)}`,
        { method: 'DELETE' },
      )
    } catch (err) {
      emitUserBlocked('session_kill_failed', {
        session_id: sessionId,
        ...requestContext(err),
        error: err instanceof Error ? err.message : String(err),
      })
      return
    }
    if (!r.ok) {
      emitUserBlocked('session_kill_failed', {
        session_id: sessionId,
        endpoint: 'session_kill',
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

  operatorEventStreamUrl: (
    context: string | null,
    unmatched: boolean,
  ): string => {
    const params = new URLSearchParams()
    if (context) params.set('context', context)
    if (unmatched) params.set('unmatched', 'true')
    const query = params.toString()
    return withToken(`/api/v2/operator/events${query ? `?${query}` : ''}`)
  },

  listChaosRules: async (sessionId: string): Promise<ChaosRule[]> => {
    const r = await request('chaos_rules', chaosRulesPath(sessionId))
    if (!r.ok) {
      if (r.status === HTTP_NOT_FOUND) return []
      throw new ApiError('chaos_rules', await chaosErrorMessage(r), r.status)
    }
    return (await r.json()) as ChaosRule[]
  },

  createChaosRule: async (
    sessionId: string,
    rule: ChaosRuleRequest,
  ): Promise<ChaosRule> => {
    const r = await request('chaos_rule_create', chaosRulesPath(sessionId), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(rule),
    })
    if (!r.ok) {
      emitUserBlocked('chaos_rule_create_failed', {
        session_id: sessionId,
        endpoint: 'chaos_rule_create',
        status: r.status,
      })
      throw new ApiError(
        'chaos_rule_create',
        await chaosErrorMessage(r),
        r.status,
      )
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
    const r = await request(
      'chaos_rule_update',
      chaosRulesPath(sessionId, ruleId),
      {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(rule),
      },
    )
    if (!r.ok) {
      emitUserBlocked('chaos_rule_update_failed', {
        session_id: sessionId,
        rule_id: ruleId,
        endpoint: 'chaos_rule_update',
        status: r.status,
      })
      throw new ApiError(
        'chaos_rule_update',
        await chaosErrorMessage(r),
        r.status,
      )
    }
    emitUserSucceeded('chaos_rule_updated', {
      session_id: sessionId,
      rule_id: ruleId,
    })
    return (await r.json()) as ChaosRule
  },

  deleteChaosRule: async (sessionId: string, ruleId: string): Promise<void> => {
    const r = await request(
      'chaos_rule_delete',
      chaosRulesPath(sessionId, ruleId),
      { method: 'DELETE' },
    )
    if (!r.ok) {
      emitUserBlocked('chaos_rule_delete_failed', {
        session_id: sessionId,
        rule_id: ruleId,
        endpoint: 'chaos_rule_delete',
        status: r.status,
      })
      throw new ApiError(
        'chaos_rule_delete',
        await chaosErrorMessage(r),
        r.status,
      )
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
    const r = await request('operator_sessions', path)
    if (!r.ok) {
      throw new ApiError(
        'operator_sessions',
        `Failed to fetch operator sessions: ${r.status} ${r.statusText}`,
        r.status,
      )
    }
    const data = (await r.json()) as OperatorSessionsResponse
    return data
  },

  // Why one preview is in its current phase.
  getPreviewDetail: async (
    id: string,
    context: string | null,
    namespace: string | null,
    logs: boolean,
  ): Promise<PreviewDetail | null> => {
    const params = new URLSearchParams()
    if (context) params.set('context', context)
    if (namespace) params.set('namespace', namespace)
    if (logs) params.set('logs', 'true')
    const qs = params.toString()
    const base = `/api/v2/operator/previews/${encodeURIComponent(id)}`
    const r = await request('preview_detail', qs ? `${base}?${qs}` : base)
    // A preview the operator has already cleaned up is gone, not an error worth surfacing.
    if (r.status === HTTP_NOT_FOUND) return null
    if (!r.ok) {
      throw new ApiError(
        'preview_detail',
        `Failed to fetch preview detail: ${r.status} ${r.statusText}`,
        r.status,
      )
    }
    return (await r.json()) as PreviewDetail
  },

  getOperatorLicense: async (
    context: string | null,
  ): Promise<OperatorLicense | null> => {
    const r = await request(
      'operator_license',
      `/api/v2/operator/license${contextParam(context)}`,
    )
    if (!r.ok) return null
    const data = (await r.json()) as OperatorLicense
    return data
  },

  listContexts: async (): Promise<ContextsResponse> => {
    const r = await request('kube_contexts', '/api/v2/kube/contexts')
    if (!r.ok)
      throw new ApiError(
        'kube_contexts',
        `Failed to fetch contexts: ${r.status} ${r.statusText}`,
        r.status,
      )
    const data = (await r.json()) as ContextsResponse
    return data
  },

  listNamespaces: async (
    context: string | null,
  ): Promise<NamespacesResponse> => {
    const r = await request(
      'kube_namespaces',
      `/api/v2/kube/namespaces${contextParam(context)}`,
    )
    if (!r.ok)
      throw new ApiError(
        'kube_namespaces',
        `Failed to fetch namespaces: ${r.status} ${r.statusText}`,
        r.status,
      )
    const data = (await r.json()) as NamespacesResponse
    return data
  },

  currentUser: async (
    context: string | null,
  ): Promise<{ k8sUsername: string | null }> => {
    const r = await request(
      'kube_user',
      `/api/v2/kube/user${contextParam(context)}`,
    )
    if (!r.ok) {
      emitUserBlocked('me_fetch_failed', {
        endpoint: 'kube_user',
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
