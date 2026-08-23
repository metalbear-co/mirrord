import type { OperatorPreviewSession, OperatorSessionSummary } from './types'

const MS_PER_SEC = 1000
const SECS_PER_MIN = 60
const MINS_PER_HOUR = 60
const SECS_PER_HOUR = 3600
const SECS_PER_DAY = 86400

// Owner the operator gives the preview entries it folds into its session list. Reading previews
// from `previewSessions` is what identifies them; this is the fallback for operators that don't
// report that list.
const PREVIEW_OWNER_USERNAME = 'preview-env'

export function isPreviewSession(session: OperatorSessionSummary): boolean {
  return (
    session.preview !== undefined ||
    session.owner.username === PREVIEW_OWNER_USERNAME
  )
}

// Builds the cluster session list the sidebar renders, taking preview environments from
// `previewSessions` and dropping the entries the operator folds into `sessions` for clients that
// read them from there, so previews aren't listed twice.
export function withPreviewSessions(
  sessions: OperatorSessionSummary[],
  previews: OperatorPreviewSession[] | undefined,
): OperatorSessionSummary[] {
  if (!previews?.length) return sessions

  const previewIds = new Set(previews.map((preview) => preview.id))

  return sessions
    .filter((session) => !previewIds.has(session.id))
    .concat(previews.map(previewAsSession))
}

function previewAsSession(
  preview: OperatorPreviewSession,
): OperatorSessionSummary {
  return {
    id: preview.id,
    key: preview.key,
    namespace: preview.namespace,
    owner: {
      username: PREVIEW_OWNER_USERNAME,
      k8sUsername: PREVIEW_OWNER_USERNAME,
    },
    target: preview.target,
    createdAt: preview.createdAt,
    ...(preview.durationSecs === undefined
      ? {}
      : { durationSecs: preview.durationSecs }),
    preview,
  }
}

export type PreviewTone = 'live' | 'pending' | 'idle' | 'failed'

export function previewPhaseTone(preview: OperatorPreviewSession): PreviewTone {
  switch (preview.phase) {
    case 'ready':
      return 'live'
    case 'initializing':
    case 'waiting':
      return 'pending'
    case 'idle':
      return 'idle'
    case 'failed':
      return 'failed'
    case 'unknown':
      return 'idle'
  }
}

export function previewPhaseLabel(
  preview: OperatorPreviewSession,
): string | null {
  switch (preview.phase) {
    case 'idle':
      return preview.idleSecs === undefined
        ? 'idle'
        : `idle ${formatDurationSecs(preview.idleSecs)}`
    case 'initializing':
    case 'waiting':
    case 'failed':
      return preview.phase
    case 'ready':
    case 'unknown':
      return null
  }
}

export function formatUptime(startedAt: string): string {
  const parsed = /^\d+$/.test(startedAt)
    ? Number(startedAt) * MS_PER_SEC
    : new Date(startedAt).getTime()
  if (!Number.isFinite(parsed)) return '—'
  const diff = Date.now() - parsed
  return formatDurationSecs(Math.floor(diff / MS_PER_SEC))
}

function formatDurationSecs(secs: number): string {
  const seconds = Math.max(0, Math.floor(secs))
  const minutes = Math.floor(seconds / SECS_PER_MIN)
  const hours = Math.floor(minutes / MINS_PER_HOUR)
  if (hours > 0) return `${hours}h ${minutes % MINS_PER_HOUR}m`
  if (minutes > 0) return `${minutes}m ${seconds % SECS_PER_MIN}s`
  return `${seconds}s`
}

export function relativeTimeFromIso(iso: string): string {
  const t = new Date(iso).getTime()
  if (!Number.isFinite(t)) return ''
  const diff = (Date.now() - t) / MS_PER_SEC
  if (diff < SECS_PER_MIN) return `${Math.max(0, Math.floor(diff))}s`
  if (diff < SECS_PER_HOUR) return `${Math.floor(diff / SECS_PER_MIN)}m`
  if (diff < SECS_PER_DAY) return `${Math.floor(diff / SECS_PER_HOUR)}h`
  return `${Math.floor(diff / SECS_PER_DAY)}d`
}

export function firstName(full: string): string {
  const first = full.trim().split(/\s+/)[0]
  if (first) return first
  return full
}

// Expects `value` to be an array; logs a warning and returns `[]` if it isn't.
// Used to defensively parse untyped JSON fields from the session monitor API,
// so a malformed response doesn't crash the component.
export function expectArray<T>(
  value: unknown,
  fieldName: string,
  context?: unknown,
): T[] {
  if (Array.isArray(value)) return value as T[]
  console.warn(
    `Session info missing expected \`${fieldName}\` array`,
    context ?? value,
  )
  return []
}

// Placeholder shown in the session metadata strip for a field that's supported but has no value
// for this particular session (e.g. no container pinned, no HTTP filter configured).
export const NOT_SET = '—'

function digConfig(config: unknown, path: string[]): unknown {
  let cursor: unknown = config
  for (const key of path) {
    if (typeof cursor !== 'object' || cursor === null) return undefined
    cursor = (cursor as Record<string, unknown>)[key]
  }
  return cursor
}

// `session.config` is a diff against mirrord's defaults (see `config_as_diff` in
// `internal_proxy.rs`), so an unset feature is simply absent from the tree rather than present
// with a default value — every lookup here has to tolerate a missing path at any depth.

// Own-session equivalent of the shared-operator view's `describeFilter`, extended to also cover
// `method_filter`/`header_filter_jq`/`body_filter` — variants `HttpFilterConfig` supports that the
// operator's own `httpFilter` summary doesn't carry at all, so only the own view can show them.
export function extractHttpFilterSummary(config: unknown): string | null {
  const filter = digConfig(config, [
    'feature',
    'network',
    'incoming',
    'http_filter',
  ])
  if (typeof filter !== 'object' || filter === null) return null
  const f = filter as Record<string, unknown>
  if (typeof f['header_filter'] === 'string')
    return `header: ${f['header_filter']}`
  if (typeof f['path_filter'] === 'string') return `path: ${f['path_filter']}`
  if (typeof f['method_filter'] === 'string')
    return `method: ${f['method_filter']}`
  if (typeof f['header_filter_jq'] === 'string')
    return `header (jq): ${f['header_filter_jq']}`
  if (f['body_filter'] !== undefined && f['body_filter'] !== null)
    return 'body filter'
  if (Array.isArray(f['all_of']) && f['all_of'].length > 0) {
    return `${f['all_of'].length} filters (all)`
  }
  if (Array.isArray(f['any_of']) && f['any_of'].length > 0) {
    return `${f['any_of'].length} filters (any)`
  }
  return null
}

// The operator's own live `queueSplits` (`OperatorQueueSplits`) only ever reports sqs/rabbitmq/
// kafka, so those three stay required to keep that type assignable here without an adapter. The
// other 5 of mirrord's 8 queue-splitting systems are local-config-only today (the operator
// doesn't report live counts for them yet), so they're optional and default to 0.
export interface QueueSplitCounts {
  sqs: number
  rabbitmq: number
  kafka: number
  gcpPubSub?: number
  redisPubSub?: number
  azureServiceBus?: number
  temporal?: number
  bullMq?: number
}

// Maps each `QueueFilter::queue_type` tag (see `split_queues.rs`) to its `QueueSplitCounts` key
// and display label. Order here is display order.
const QUEUE_TYPE_TAGS: {
  tag: string
  key: keyof QueueSplitCounts
  label: string
}[] = [
  { tag: 'SQS', key: 'sqs', label: 'SQS' },
  { tag: 'RMQ', key: 'rabbitmq', label: 'RabbitMQ' },
  { tag: 'Kafka', key: 'kafka', label: 'Kafka' },
  { tag: 'GCPPubSub', key: 'gcpPubSub', label: 'GCP Pub/Sub' },
  { tag: 'RedisPubSub', key: 'redisPubSub', label: 'Redis Pub/Sub' },
  {
    tag: 'AzureServiceBus',
    key: 'azureServiceBus',
    label: 'Azure Service Bus',
  },
  { tag: 'Temporal', key: 'temporal', label: 'Temporal' },
  { tag: 'BullMQ', key: 'bullMq', label: 'BullMQ' },
]

// Own-session equivalent of the shared-operator view's live `queueSplits` count. This one is
// read from the local config's *requested* `split_queues` entries, not a live count from the
// operator — the two can differ while the operator is still provisioning. Returns `null` when
// nothing is configured (distinct from all-zero, which can't actually happen here since an empty
// config array is indistinguishable from an absent one after diffing).
//
// `SplitQueuesConfig`'s custom `Serialize` emits the classic map form (`{queue_id: {...}}`)
// whenever every queue id is unique, and only falls back to a plain array when an id repeats (a
// map can't represent that) — see `SplitQueuesConfig::serialize` in `split_queues.rs`. Both
// shapes have to be accepted here, since the common case (unique ids) is the map form.
export function extractQueueSplitsFromConfig(
  config: unknown,
): QueueSplitCounts | null {
  const entries = digConfig(config, ['feature', 'split_queues'])
  const list: unknown[] = Array.isArray(entries)
    ? entries
    : typeof entries === 'object' && entries !== null
      ? Object.values(entries)
      : []
  if (list.length === 0) return null

  const counts: QueueSplitCounts = {
    sqs: 0,
    rabbitmq: 0,
    kafka: 0,
    gcpPubSub: 0,
    redisPubSub: 0,
    azureServiceBus: 0,
    temporal: 0,
    bullMq: 0,
  }
  for (const entry of list) {
    const queueType = (entry as Record<string, unknown> | null)?.['queue_type']
    const match = QUEUE_TYPE_TAGS.find((t) => t.tag === queueType)
    if (match) counts[match.key] = (counts[match.key] ?? 0) + 1
  }
  return counts
}

// Shared by both views: the operator reports live `{sqs, rabbitmq, kafka}` counts, the own view
// derives the same shape (plus the 5 config-only systems) from local config — either way this
// renders them the same way.
export function formatQueueSplits(counts: QueueSplitCounts): string {
  return QUEUE_TYPE_TAGS.map((t) => ({ n: counts[t.key] ?? 0, label: t.label }))
    .filter((t) => t.n > 0)
    .map((t) => `${t.label} ${t.n}`)
    .join(' · ')
}

// Sum across all 8 systems, not just sqs/rabbitmq/kafka — used to decide whether there's
// anything to render at all before calling `formatQueueSplits`.
export function totalQueueSplits(counts: QueueSplitCounts): number {
  return QUEUE_TYPE_TAGS.reduce((sum, t) => sum + (counts[t.key] ?? 0), 0)
}

// `session.target` is the CLI's own `Target` display string, `<kind>/<name>[/container/<name>]`
// (see `mirrord-config`'s `Target` Display impl) — parsing it back out is simpler and more
// reliable than re-deriving the same information from the config diff.
export function extractContainerFromTarget(target: string): string | null {
  const match = /\/container\/(.+)$/.exec(target)
  return match?.[1] ?? null
}

// intproxy's outgoing_connection events can carry the port both inside `address`
// and as `port`, so joining them naively doubles it ("10.0.0.1:80:80").
export function formatHostPort(address: string, port: number): string {
  const suffix = `:${port}`
  return address.endsWith(suffix) ? address : `${address}${suffix}`
}

export function stripPortSuffix(address: string, port: number): string {
  const suffix = `:${port}`
  return address.endsWith(suffix) ? address.slice(0, -suffix.length) : address
}
