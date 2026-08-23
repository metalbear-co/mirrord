const MS_PER_SEC = 1000
const SECS_PER_MIN = 60
const MINS_PER_HOUR = 60
const SECS_PER_HOUR = 3600
const SECS_PER_DAY = 86400

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

// Own-session equivalent of the shared-operator view's `describeFilter`: reads the HTTP filter
// the user configured locally, matching the same fields (header/path/all_of/any_of) so both
// views describe a filter the same way regardless of where the session came from.
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
  if (Array.isArray(f['all_of']) && f['all_of'].length > 0) {
    return `${f['all_of'].length} filters (all)`
  }
  if (Array.isArray(f['any_of']) && f['any_of'].length > 0) {
    return `${f['any_of'].length} filters (any)`
  }
  return null
}

export interface QueueSplitCounts {
  sqs: number
  rabbitmq: number
  kafka: number
}

// Own-session equivalent of the shared-operator view's live `queueSplits` count. This one is
// read from the local config's *requested* `split_queues` entries, not a live count from the
// operator — the two can differ while the operator is still provisioning. Returns `null` when
// nothing is configured (distinct from all-zero, which can't actually happen here since an empty
// config array is indistinguishable from an absent one after diffing).
export function extractQueueSplitsFromConfig(
  config: unknown,
): QueueSplitCounts | null {
  const entries = digConfig(config, ['feature', 'split_queues'])
  if (!Array.isArray(entries) || entries.length === 0) return null
  const counts: QueueSplitCounts = { sqs: 0, rabbitmq: 0, kafka: 0 }
  for (const entry of entries) {
    const queueType = (entry as Record<string, unknown> | null)?.['queue_type']
    if (queueType === 'SQS') counts.sqs += 1
    else if (queueType === 'Kafka') counts.kafka += 1
    else if (queueType === 'RMQ') counts.rabbitmq += 1
  }
  return counts
}

// Shared by both views: the operator reports live `{sqs, rabbitmq, kafka}` counts, the own view
// derives the same shape from local config — either way this renders them the same way.
export function formatQueueSplits(counts: QueueSplitCounts): string {
  const parts: string[] = []
  if (counts.sqs > 0) parts.push(`SQS ${counts.sqs}`)
  if (counts.rabbitmq > 0) parts.push(`RabbitMQ ${counts.rabbitmq}`)
  if (counts.kafka > 0) parts.push(`Kafka ${counts.kafka}`)
  return parts.join(' · ')
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
