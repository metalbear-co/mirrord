import type { MonitorEvent } from '../../types'
import { EventType, type EventTypeValue } from '../../eventTypes'
import { strings } from '../../strings'

export interface ParsedEvent {
  type: EventTypeValue
  summary: string
  rawData?: unknown
}

// HTTP bodies (and fields nested inside them) are frequently JSON that has itself been
// serialized into a string. Walk the value and parse any object/array-looking string back
// into structure so the detail dialog renders readable nested JSON at every level; strings
// that aren't valid JSON (HTML, plain text, quoted scalars) are left exactly as they are.
function deepDecode(value: unknown): unknown {
  if (typeof value === 'string') {
    const trimmed = value.trim()
    if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
      try {
        return deepDecode(JSON.parse(trimmed))
      } catch {}
    }
    return value
  }
  if (Array.isArray(value)) return value.map(deepDecode)
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, deepDecode(v)]))
  }
  return value
}

function decodeBody<T extends { body: string }>(event: T): T | (Omit<T, 'body'> & { body: unknown }) {
  const decoded = deepDecode(event.body)
  return decoded === event.body ? event : { ...event, body: decoded }
}

// Returns null for event kinds that should not be shown in the event log
// (port_subscription, env_var — surfaced elsewhere in the UI) or for malformed events.
export function parseEvent(event: MonitorEvent): ParsedEvent | null {
  try {
    switch (event.type) {
      case EventType.FileOp:
        return { type: EventType.FileOp, summary: `${event.operation}: ${event.path || strings.events.unknownPath}` }
      case EventType.DnsQuery:
        return { type: EventType.DnsQuery, summary: `DNS lookup: ${event.host}` }
      case EventType.IncomingRequest:
        return {
          type: EventType.IncomingRequest,
          summary: `${event.method} ${event.host}${event.path}`,
          rawData: event.headers?.length ? event : undefined,
        }
      case EventType.IncomingResponse:
        return {
          type: EventType.IncomingRequest,
          summary: `${event.method} ${event.path} → ${event.status}`,
          rawData: event,
        }
      case EventType.IncomingRequestBody:
        return {
          type: EventType.IncomingRequest,
          summary: `${event.method} ${event.path} request body (${formatBytes(event.bytes)}${event.truncated ? ', truncated' : ''})`,
          rawData: decodeBody(event),
        }
      case EventType.IncomingResponseBody:
        return {
          type: EventType.IncomingRequest,
          summary: `${event.method} ${event.path} → ${event.status} body (${formatBytes(event.bytes)}${event.truncated ? ', truncated' : ''})`,
          rawData: decodeBody(event),
        }
      case EventType.OutgoingConnection:
        return { type: EventType.OutgoingConnection, summary: `Outgoing: ${event.address}:${event.port}` }
      case EventType.PortSubscription:
      case EventType.EnvVar:
        return null
      case EventType.LayerConnected:
        return { type: EventType.LayerConnected, summary: `Process connected: ${event.process_name} (PID ${event.pid})` }
      case EventType.LayerDisconnected:
        return { type: EventType.LayerDisconnected, summary: `Process disconnected (PID ${event.pid})` }
      default:
        return null
    }
  } catch (err) {
    console.warn('Failed to parse event', event, err)
    return null
  }
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

export function formatTime24(date: Date): string {
  // Pass undefined locale so the browser uses the user's default locale.
  return date.toLocaleTimeString(undefined, { hour12: false })
}
