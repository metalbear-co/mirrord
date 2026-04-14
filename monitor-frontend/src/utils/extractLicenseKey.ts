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
