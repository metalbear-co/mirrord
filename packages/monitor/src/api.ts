import type { OperatorSessionsResponse, SessionInfo } from './types'

export const api = {
  listSessions: async (): Promise<SessionInfo[]> => {
    const r = await fetch('/api/sessions')
    if (!r.ok) throw new Error(`Failed to fetch sessions: ${r.status} ${r.statusText}`)
    return r.json()
  },

  getSession: async (sessionId: string): Promise<SessionInfo | null> => {
    const r = await fetch(`/api/sessions/${encodeURIComponent(sessionId)}`)
    return r.ok ? r.json() : null
  },

  killSession: (sessionId: string): Promise<Response> =>
    fetch(`/api/sessions/${encodeURIComponent(sessionId)}/kill`, { method: 'POST' }),

  eventStreamUrl: (sessionId: string): string =>
    `/api/sessions/${encodeURIComponent(sessionId)}/events`,

  listOperatorSessions: async (): Promise<OperatorSessionsResponse> => {
    const r = await fetch('/api/operator-sessions')
    if (!r.ok) throw new Error(`Failed to fetch operator sessions: ${r.status} ${r.statusText}`)
    return r.json()
  },

  wsUrl: (): string =>
    `${window.location.protocol === 'https:' ? 'wss:' : 'ws:'}//${window.location.host}/ws`,
}
