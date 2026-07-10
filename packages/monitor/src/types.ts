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
  context?: string | null
}

export interface KubeContext {
  name: string
  namespace: string | null
}

export interface ContextsResponse {
  current: string | null
  contexts: KubeContext[]
}

export interface NamespacesResponse {
  context: string | null
  namespaces: string[]
}

// Matches Rust MonitorEvent with #[serde(tag = "type", rename_all = "snake_case")]
export type MonitorEvent =
  | { type: 'file_op'; path: string | null; operation: string }
  | { type: 'dns_query'; host: string }
  | { type: 'incoming_request'; id: string; method: string; path: string; host: string; port?: number; http_version?: string; headers?: [string, string][] }
  | { type: 'incoming_response'; id: string; status: number; method: string; path: string; http_version?: string; headers?: [string, string][] }
  | { type: 'incoming_request_body'; id: string; method: string; path: string; body: string; truncated: boolean; bytes: number }
  | { type: 'incoming_response_body'; id: string; status: number; method: string; path: string; body: string; truncated: boolean; bytes: number }
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

// Reachability of the operator, as the sidebar consumes it. The v2 server only ever produces
// `watching`/`unavailable` (each poll is a one-shot fetch that either reached the operator or
// didn't); `not_started`/`error` remain in the type for the sidebar's transient-state handling but
// are not emitted.
export const OPERATOR_WATCH = {
  NotStarted: 'not_started',
  Watching: 'watching',
  Error: 'error',
  Unavailable: 'unavailable',
} as const

export type OperatorWatchStatus =
  | { status: typeof OPERATOR_WATCH.NotStarted }
  | { status: typeof OPERATOR_WATCH.Watching }
  | { status: typeof OPERATOR_WATCH.Error; message: string }
  | { status: typeof OPERATOR_WATCH.Unavailable; reason: string }

export interface OperatorLicense {
  fingerprint: string | null
  organization: string
}

// v2 `GET /api/v2/operator/sessions?context&namespace`
export interface OperatorSessionsResponse {
  context: string | null
  status: 'available' | 'unavailable'
  reason?: string
  sessions: OperatorSessionSummary[]
}

