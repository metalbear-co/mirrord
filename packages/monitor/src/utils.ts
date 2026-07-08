export function formatUptime(startedAt: string): string {
  const parsed = /^\d+$/.test(startedAt)
    ? Number(startedAt) * 1000
    : new Date(startedAt).getTime();
  if (!Number.isFinite(parsed)) return "—";
  const diff = Date.now() - parsed;
  return formatDurationSecs(Math.floor(diff / 1000));
}

export function formatDurationSecs(secs: number): string {
  const seconds = Math.max(0, Math.floor(secs));
  const minutes = Math.floor(seconds / 60);
  const hours = Math.floor(minutes / 60);
  if (hours > 0) return `${hours}h ${minutes % 60}m`;
  if (minutes > 0) return `${minutes}m ${seconds % 60}s`;
  return `${seconds}s`;
}

export function relativeTimeFromIso(iso: string): string {
  const t = new Date(iso).getTime();
  if (!Number.isFinite(t)) return "";
  const diff = (Date.now() - t) / 1000;
  if (diff < 60) return `${Math.max(0, Math.floor(diff))}s`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m`;
  if (diff < 86400) return `${Math.floor(diff / 3600)}h`;
  return `${Math.floor(diff / 86400)}d`;
}

export function firstName(full: string): string {
  return full.trim().split(/\s+/)[0] || full;
}

// Expects `value` to be an array; logs a warning and returns `[]` if it isn't.
// Used to defensively parse untyped JSON fields from the session monitor API,
// so a malformed response doesn't crash the component.
export function expectArray<T>(
  value: unknown,
  fieldName: string,
  context?: unknown,
): T[] {
  if (Array.isArray(value)) return value as T[];
  console.warn(
    `Session info missing expected \`${fieldName}\` array`,
    context ?? value,
  );
  return [];
}

// Session config may carry the license `key` as either a plain string or an
// object shape (e.g. { value: "..." }); flatten it to a displayable string.
export function extractLicenseKey(config: unknown): string | null {
  const rawKey = (config as Record<string, unknown> | null)?.key;
  if (!rawKey) return null;
  if (typeof rawKey === "string") return rawKey;
  if (typeof rawKey === "object") {
    const firstValue = Object.values(rawKey as Record<string, unknown>)[0];
    return firstValue ? String(firstValue) : null;
  }
  return String(rawKey);
}
