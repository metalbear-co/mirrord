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
  config?: Record<string, unknown>
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
  | { type: 'incoming_request'; method: string; path: string; host: string }
  | { type: 'outgoing_connection'; address: string; port: number }
  | { type: 'port_subscription'; port: number; mode: string }
  | { type: 'env_var'; vars: string[] }
  | { type: 'layer_connected'; pid: number; process_name: string }
  | { type: 'layer_disconnected'; pid: number }

interface OperatorSessionHttpFilter {
  headerFilter?: string | null
  pathFilter?: string | null
  allOf?: OperatorSessionHttpFilter[] | null
  anyOf?: OperatorSessionHttpFilter[] | null
}

export interface OperatorSessionOwner {
  username: string
  k8sUsername: string
}

interface OperatorSessionTarget {
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

export interface QueueSplitFilterInfo {
  id: string
  queueType: string
  messageFilter?: Record<string, string>
  jqFilter?: string | null
}

export interface QueueSplitQueueInfo {
  id: string
  type: string
  queue?: string | null
  topic?: string | null
  consumerGroup?: string | null
  subscription?: string | null
}

export interface QueueSplitPodInfo {
  name: string
  patched: boolean
  ready: boolean
}

export interface QueueSplitView {
  name: string
  session: string
  phase: string
  message?: string | null
  filters?: QueueSplitFilterInfo[]
  queues?: QueueSplitQueueInfo[]
  targetPods?: QueueSplitPodInfo[]
}

export interface QueueSplitsResponse {
  context: string | null
  status: 'available' | 'unavailable'
  reason?: string | null
  splits: QueueSplitView[]
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

// Chaos rules ("mirrord chaos"), scoped to a local exec session (see mirrord-intproxy's
// `session_monitor::chaos::rules`). Only the Tcp selector and the Latency/ConnectionError effects
// are implemented server-side today — Http and Fs selectors exist in the Rust enum but are
// rejected with an "unimplemented" error, so we don't model them here yet.
export type ConnectionErrorType = 'reset' | 'timed_out' | 'refused'

export interface ChaosEffectLatency {
  read_ms?: number | undefined
  write_ms?: number | undefined
  jitter_ms?: number | undefined
}

export interface ChaosEffectConnectionError {
  error_type: ConnectionErrorType
  after_ms?: number
}

export type ChaosEffect =
  | { latency: ChaosEffectLatency }
  | { connection_error: ChaosEffectConnectionError }

export type ChaosSelector =
  | { type: 'tcp'; upstream: string; percentage: number; effect: ChaosEffect }
  | { type: 'none' }

export interface ChaosRule {
  id: string
  name?: string | null
  priority: number
  selector: ChaosSelector
  hit_count: number
}

export type ChaosEffectRequest =
  | { latency: ChaosEffectLatency }
  | {
      connection_error: {
        type: ConnectionErrorType
        after_ms?: number | undefined
      }
    }

export interface ChaosRuleRequest {
  name?: string | null | undefined
  priority?: number | null | undefined
  effect: ChaosEffectRequest
  selector: {
    upstream: string
    percentage?: number | null | undefined
  }
}

// Client-side view of a chaos rule. The server has no pause bit, so a paused rule
// is deleted server-side and kept here with its config and frozen hit count;
// re-arming recreates it (`serverId` changes, `key` stays stable for React).
export type ChaosEffectKind = 'latency' | ConnectionErrorType

export interface ClientChaosRule {
  key: string
  serverId: string | null
  name: string
  upstream: string
  effectKind: ChaosEffectKind
  readMs: number
  writeMs: number
  jitterMs: number
  afterMs: number
  percentage: number
  priority: number
  armed: boolean
  hits: number
  serverHits: number
  spark: SparkBucket[]
  flash: boolean
}

export interface SparkBucket {
  id: number
  value: number
}
