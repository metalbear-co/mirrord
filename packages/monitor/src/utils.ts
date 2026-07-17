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

function stringifyPrimitive(value: unknown): string | null {
  if (typeof value === 'string') return value || null
  if (
    typeof value === 'number' ||
    typeof value === 'boolean' ||
    typeof value === 'bigint'
  ) {
    return value ? String(value) : null
  }
  return null
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

// Session config may carry the license `key` as either a plain string or an
// object shape (e.g. { value: "..." }); flatten it to a displayable string.
export function extractLicenseKey(config: unknown): string | null {
  const rawKey = (config as Record<string, unknown> | null)?.['key']
  if (!rawKey) return null
  if (typeof rawKey === 'string') return rawKey
  if (typeof rawKey === 'object') {
    const firstValue = Object.values(rawKey as Record<string, unknown>)[0]
    return stringifyPrimitive(firstValue)
  }
  return stringifyPrimitive(rawKey)
}
