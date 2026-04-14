export function formatUptime(startedAt: string): string {
  const parsed = /^\d+$/.test(startedAt) ? Number(startedAt) * 1000 : new Date(startedAt).getTime()
  const diff = Date.now() - parsed
  const seconds = Math.floor(diff / 1000)
  const minutes = Math.floor(seconds / 60)
  const hours = Math.floor(minutes / 60)
  if (hours > 0) return `${hours}h ${minutes % 60}m`
  if (minutes > 0) return `${minutes}m ${seconds % 60}s`
  return `${seconds}s`
}

// Session config may carry the license `key` as either a plain string or an
// object shape (e.g. { value: "..." }); flatten it to a displayable string.
export function extractLicenseKey(config: unknown): string | null {
  const rawKey = (config as Record<string, unknown> | null)?.key
  if (!rawKey) return null
  if (typeof rawKey === 'string') return rawKey
  if (typeof rawKey === 'object') {
    const firstValue = Object.values(rawKey as Record<string, unknown>)[0]
    return firstValue ? String(firstValue) : null
  }
  return String(rawKey)
}
