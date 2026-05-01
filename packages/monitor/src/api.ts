import type { OperatorSessionsResponse, SessionInfo } from './types'

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

export const api = {
  listSessions: async (): Promise<SessionInfo[]> => {
    const r = await fetch(withToken('/api/sessions'), { credentials: 'include' })
    if (!r.ok) throw new Error(`Failed to fetch sessions: ${r.status} ${r.statusText}`)
    return r.json()
  },

  getSession: async (sessionId: string): Promise<SessionInfo | null> => {
    const r = await fetch(withToken(`/api/sessions/${encodeURIComponent(sessionId)}`), {
      credentials: 'include',
    })
    return r.ok ? r.json() : null
  },

  killSession: (sessionId: string): Promise<Response> =>
    fetch(withToken(`/api/sessions/${encodeURIComponent(sessionId)}/kill`), {
      method: 'POST',
      credentials: 'include',
    }),

  eventStreamUrl: (sessionId: string): string =>
    withToken(`/api/sessions/${encodeURIComponent(sessionId)}/events`),

  listOperatorSessions: async (): Promise<OperatorSessionsResponse> => {
    const r = await fetch(withToken('/api/operator-sessions'), {
      credentials: 'include',
    })
    if (!r.ok)
      throw new Error(
        `Failed to fetch operator sessions: ${r.status} ${r.statusText}`
      )
    return r.json()
  },

  currentUser: async (): Promise<{ k8sUsername: string | null }> => {
    const r = await fetch(withToken('/api/me'), { credentials: 'include' })
    if (!r.ok) return { k8sUsername: null }
    const data = (await r.json()) as { k8sUsername?: string | null }
    return { k8sUsername: data.k8sUsername ?? null }
  },

  wsUrl: (): string => {
    const scheme = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
    const base = `${scheme}//${window.location.host}/ws`
    return authToken ? `${base}?token=${encodeURIComponent(authToken)}` : base
  },
}
