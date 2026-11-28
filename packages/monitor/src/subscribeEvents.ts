// Wire shape and row derivation for the operator's interception events, as served by
// `/api/v2/operator/events`. `data` is an externally-tagged union with one key naming the type. A
// type this build does not know still gets a row, labelled by its tag, so nothing the operator
// reports goes uncounted.

/** The workload an event was intercepted on, as a mirrord target path names it: `kind/name`. */
export interface WorkloadTarget {
  /** mirrord's lowercase target type, such as `deployment`, `rollout` or `targetless`. */
  kind: string
  /** The workload's name, the label selector of a `label` target, or empty for `targetless`. */
  name: string
}

export interface SubscribeEventRow {
  /** Arrival order: the React key, and the order rows are shown in. */
  seq: number
  timestamp: string
  sessionKey: string
  /** The cluster that intercepted it on a multi-cluster primary; empty elsewhere. */
  cluster: string
  /** Namespace of the intercepted workload; empty from an operator that doesn't name it. */
  namespace: string
  /** The intercepted workload; `null` from an operator that doesn't name it. */
  target: WorkloadTarget | null
  /** The intercepted workload's name, which every operator sends. */
  serviceName: string
  /** The payload tag, or the broker for a queue message. */
  type: string
  source: string
  status: string
  /** Whether this row is a message no session's filter claimed. */
  filtered: boolean
  /** Identifies an HTTP request and the response answering it; empty for anything else. */
  correlation: string
  /** Whether this row is still waiting for the response that completes it. */
  awaitingResponse: boolean
}

/** Every type the table has a name for. */
const TYPE_LABELS: Record<string, string> = {
  http: 'HTTP',
  http_response: 'HTTP response',
  kafka_message: 'Kafka',
  redis_message: 'Redis Pub/Sub',
  lagged: 'Lagged',
  sqs: 'SQS',
  rmq: 'RabbitMQ',
  gcppubsub: 'GCP Pub/Sub',
  bullmq: 'BullMQ',
  nats: 'NATS',
  natspubsub: 'NATS Pub/Sub',
  azure_service_bus: 'Azure Service Bus',
}

/** What the operator did with a message, as the Status column reads it. */
const MODE_LABELS: Record<string, string> = {
  steal: 'stolen',
  mirror: 'mirrored',
  filtered: 'filtered',
}

export function labelForType(type: string): string | null {
  return TYPE_LABELS[type] ?? null
}

/**
 * The workload as the table names it: `kind/name` when the event has a `target` (only `kind` for
 * `targetless`), else the `service_name` that older operators send.
 */
export function targetLabel(row: SubscribeEventRow): string {
  if (!row.target) return row.serviceName
  const { kind, name } = row.target
  return name === '' ? kind : `${kind}/${name}`
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null
}

function asString(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

function asTarget(value: unknown): WorkloadTarget | null {
  const target = asRecord(value)
  const kind = asString(target?.['kind'])
  return kind === '' ? null : { kind, name: asString(target?.['name']) }
}

interface Described {
  type: string
  source: string
  status: string
}

function describe(
  tag: string,
  payload: Record<string, unknown>,
  mode: string,
): Described | null {
  const base = { type: tag, source: '', status: MODE_LABELS[mode] ?? '' }

  switch (tag) {
    case 'http_request':
      return {
        ...base,
        type: 'http',
        source:
          `${asString(payload['method'])} ${asString(payload['uri'])}`.trim(),
      }
    case 'http_response':
      return {
        ...base,
        status:
          typeof payload['status'] === 'number'
            ? String(payload['status'])
            : '',
      }
    case 'queue_message': {
      const broker = asString(payload['queue_type'])
      if (broker === '') return null

      return {
        ...base,
        type: broker,
        source: asString(payload['queue_name']),
      }
    }
    case 'kafka_message':
      return { ...base, source: asString(payload['topic']) }
    case 'redis_message':
      return { ...base, source: asString(payload['channel']) }
    case 'lagged':
      return {
        ...base,
        status: '',
        source:
          typeof payload['count'] === 'number'
            ? `${payload['count']} events dropped`
            : '',
      }
    default:
      return base
  }
}

/** Turns one streamed payload into a table row, or `null` if it isn't an event. */
export function toEventRow(
  raw: unknown,
  seq: number,
): SubscribeEventRow | null {
  const event = asRecord(raw)
  if (!event) return null

  const data = asRecord(event['data'])
  if (!data) return null

  const [tag] = Object.keys(data)
  if (tag === undefined) return null

  const payload = asRecord(data[tag]) ?? {}
  const mode = asString(payload['mode'])
  const described = describe(tag, payload, mode)
  if (!described) return null

  const connection = payload['connection_id']
  const request = payload['request_id']
  const correlation =
    typeof connection === 'number' && typeof request === 'number'
      ? `${connection}:${request}`
      : ''

  return {
    seq,
    timestamp: asString(event['timestamp']),
    sessionKey: asString(event['session_key']),
    cluster: asString(event['cluster']),
    namespace: asString(event['namespace']),
    target: asTarget(event['target']),
    serviceName: asString(event['service_name']),
    type: described.type,
    source: described.source,
    status: described.status,
    filtered: mode === 'filtered',
    correlation,
    awaitingResponse: tag === 'http_request' && correlation !== '',
  }
}
