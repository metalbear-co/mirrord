export interface SessionInfo {
  session_id: string
  target: string
  mode: 'steal' | 'mirror'
  pid: number
  process_name: string
  started_at: string
  ports: number[]
  mirrord_version: string
}

export interface SessionEvent {
  type: 'file_op' | 'dns_query' | 'http_request' | 'connection' | 'env_var'
  timestamp: number
  data: Record<string, unknown>
}

export interface WsMessage {
  type: 'session_added' | 'session_removed'
  session_id: string
  info?: SessionInfo
}
