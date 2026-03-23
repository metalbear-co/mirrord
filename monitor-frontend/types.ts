export interface ProcessInfo {
  pid: number
  process_name: string
}

export interface SessionInfo {
  session_id: string
  target: string
  started_at: string
  mirrord_version: string
  is_operator: boolean
  processes: ProcessInfo[]
  config: Record<string, unknown>
  filter: string | null
}

/** A raw event from the backend SSE stream, enriched with a local receive timestamp. */
export interface SessionEvent {
  type: string
  received_at: number
  [key: string]: unknown
}

export interface WsMessage {
  type: 'session_added' | 'session_removed'
  session_id: string
  info?: SessionInfo
}
