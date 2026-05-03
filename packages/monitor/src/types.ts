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
  key?: string | null
  namespace?: string | null
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

export interface OperatorSessionHttpFilter {
  headerFilter?: string | null
  pathFilter?: string | null
  allOf?: OperatorSessionHttpFilter[] | null
  anyOf?: OperatorSessionHttpFilter[] | null
}

export interface OperatorSessionOwner {
  username: string
  k8sUsername: string
}

export interface OperatorSessionTarget {
  kind: string
  name: string
  container: string
}

export interface OperatorLockedPort {
  port: number
  kind: string
  filter?: string | null
}

export interface OperatorQueueSplits {
  sqs: number
  rabbitmq: number
  kafka: number
}

export interface OperatorSessionSummary {
  id: string
  key: string
  namespace: string
  owner: OperatorSessionOwner
  target: OperatorSessionTarget | null
  createdAt: string
  durationSecs?: number
  lockedPorts?: OperatorLockedPort[]
  queueSplits?: OperatorQueueSplits
  httpFilter?: OperatorSessionHttpFilter | null
}

export type OperatorWatchStatus =
  | { status: 'not_started' }
  | { status: 'watching' }
  | { status: 'error'; message: string }
  | { status: 'unavailable'; reason: string }

export interface OperatorSessionsResponse {
  by_key: Record<string, OperatorSessionSummary[]>
  sessions: OperatorSessionSummary[]
  watch_status: OperatorWatchStatus
}

export type WsMessage =
  | { type: 'session_added'; session: SessionInfo }
  | { type: 'session_removed'; session_id: string }
  | { type: 'operator_session_added'; session: OperatorSessionSummary }
  | { type: 'operator_session_removed'; id: string }
  | { type: 'operator_session_updated'; session: OperatorSessionSummary }
