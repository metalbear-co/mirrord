export interface ProcessInfo {
  pid: number
  process_name: string
}

export interface PortSubscription {
  port: number
  mode: string
}

export interface SessionInfo {
  session_id: string
  target: string
  started_at: string
  mirrord_version: string
  is_operator: boolean
  processes: ProcessInfo[]
  port_subscriptions: PortSubscription[]
  config: Record<string, unknown>
}

// Matches Rust MonitorEvent with #[serde(tag = "type", rename_all = "snake_case")]
export type MonitorEvent =
  | { type: 'file_op'; path: string | null; operation: string }
  | { type: 'dns_query'; host: string }
  | { type: 'incoming_request'; method: string; path: string; host: string }
  | { type: 'outgoing_connection'; address: string; port: number }
  | { type: 'port_subscription'; port: number; mode: string }
  | { type: 'env_var'; vars: string[] }
  | { type: 'layer_connected'; pid: number; process_name: string }
  | { type: 'layer_disconnected'; pid: number }

export type WsMessage =
  | { type: 'session_added'; session: SessionInfo }
  | { type: 'session_removed'; session_id: string }
