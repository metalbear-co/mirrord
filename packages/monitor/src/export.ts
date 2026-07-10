import { strToU8, zipSync } from 'fflate'
import type { MonitorEvent, SessionInfo } from './types'
import { buildHar, describeTarget } from './har'

interface TimestampedEvent {
  event: MonitorEvent
  receivedAt: Date
}

// Exports get handed to teammates, support, and AI tools; leaked `Authorization`/`Cookie`
// values are live credentials, so exports always redact them. The detail dialog in the UI
// still shows raw values.
const SENSITIVE_HEADERS = new Set([
  'authorization',
  'proxy-authorization',
  'cookie',
  'set-cookie',
  'x-api-key',
])

export const REDACTED_VALUE = '[REDACTED by mirrord session monitor]'

const redactHeaders = (headers: [string, string][]): [string, string][] =>
  headers.map(([name, value]) =>
    SENSITIVE_HEADERS.has(name.toLowerCase()) ? [name, REDACTED_VALUE] : [name, value],
  )

export function redactEvent(event: MonitorEvent): MonitorEvent {
  if ('headers' in event && event.headers?.length) {
    return { ...event, headers: redactHeaders(event.headers) }
  }
  return event
}

export interface ExportOptions {
  // Events already evicted by the in-memory cap: the export can only contain what the page
  // still holds, so the count is recorded instead of silently claiming completeness.
  droppedEvents: number
  exportedAt: Date
}

export function buildSessionLog(
  session: SessionInfo,
  events: TimestampedEvent[],
  { droppedEvents, exportedAt }: ExportOptions,
) {
  return {
    description:
      `Session log from the mirrord session monitor: every event captured while mirrord ran ` +
      `against ${describeTarget(session)}, session ${session.session_id} ` +
      `(mirrord ${session.mirrord_version}), started ${session.started_at}, ` +
      `exported ${exportedAt.toISOString()} with ${events.length} events. ` +
      `HTTP events belonging to one request/response exchange share an "id". Sensitive header ` +
      `values (authorization, cookies, API keys) are redacted, and HTTP bodies past the ` +
      `configured capture limit are truncated and flagged.` +
      (droppedEvents > 0
        ? ` ${droppedEvents} older events were evicted by the in-memory cap before this export ` +
          `and are not included.`
        : ''),
    exported_at: exportedAt.toISOString(),
    event_count: events.length,
    dropped_events: droppedEvents,
    session,
    events: events.map(({ event, receivedAt }) => ({
      received_at: receivedAt.toISOString(),
      ...event,
    })),
  }
}

// One zip bundling the redacted session log (every captured event) and a HAR of the HTTP
// exchanges: a single download avoids the browser's multiple-download permission prompt.
export function buildExportZip(
  session: SessionInfo,
  events: TimestampedEvent[],
  options: ExportOptions,
): { filename: string; data: Uint8Array } {
  const redacted = events.map(({ event, receivedAt }) => ({
    event: redactEvent(event),
    receivedAt,
  }))
  const stamp = options.exportedAt.toISOString().replace(/[:.]/g, '-')
  const targetSlug = session.target.replace(/[^a-zA-Z0-9]+/g, '-').replace(/^-+|-+$/g, '')
  const base = `mirrord-session-${targetSlug}-${session.session_id}-${stamp}`
  const data = zipSync({
    [`${base}.json`]: strToU8(JSON.stringify(buildSessionLog(session, redacted, options), null, 2)),
    [`${base}.har`]: strToU8(
      JSON.stringify(buildHar(session, redacted, options.exportedAt), null, 2),
    ),
  })
  return { filename: `${base}.zip`, data }
}
