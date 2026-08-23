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
