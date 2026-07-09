import type { MonitorEvent } from '../../types'
import { EventType, type EventTypeValue } from '../../eventTypes'
import { strings } from '../../strings'

// Table columns for the event log. `status`/`statusTone` drive the status pill; `groupKey`
// identifies rows the "group repeats" toggle may collapse when they arrive consecutively.
export interface EventColumns {
  method: string
  path: string
  status: string
  statusTone: 'ok' | 'error' | 'muted'
}

export interface ParsedEvent {
  type: EventTypeValue
  summary: string
  rawData?: unknown
  columns: EventColumns
  groupKey: string
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

const httpStatusTone = (status: number): EventColumns['statusTone'] =>
  status >= 400 ? 'error' : 'ok'

// Returns null for event kinds that should not be shown in the event log
// (port_subscription, env_var — surfaced elsewhere in the UI) or for malformed events.
export function parseEvent(event: MonitorEvent): ParsedEvent | null {
  try {
    switch (event.type) {
      case EventType.FileOp: {
        const columns: EventColumns = {
          method: event.operation.toUpperCase(),
          path: event.path || strings.events.unknownPath,
          status: 'OK',
          statusTone: 'ok',
        }
        return {
          type: EventType.FileOp,
          summary: `${event.operation}: ${event.path || strings.events.unknownPath}`,
          columns,
          groupKey: `file:${columns.method}:${columns.path}`,
        }
      }
      case EventType.DnsQuery: {
        const columns: EventColumns = {
          method: 'QUERY',
          path: event.host,
          status: 'OK',
          statusTone: 'ok',
        }
        return {
          type: EventType.DnsQuery,
          summary: `DNS lookup: ${event.host}`,
          columns,
          groupKey: `dns:${event.host}`,
        }
      }
      case EventType.IncomingRequest: {
        const columns: EventColumns = {
          method: event.method,
          path: `${event.host}${event.path}`,
          status: '',
          statusTone: 'muted',
        }
        return {
          type: EventType.IncomingRequest,
          summary: `${event.method} ${event.host}${event.path}`,
          rawData: event.headers?.length ? event : undefined,
          columns,
          groupKey: `req:${event.method}:${event.host}${event.path}`,
        }
      }
      case EventType.IncomingResponse: {
        const columns: EventColumns = {
          method: event.method,
          path: event.path,
          status: String(event.status),
          statusTone: httpStatusTone(event.status),
        }
        return {
          type: EventType.IncomingRequest,
          summary: `${event.method} ${event.path} → ${event.status}`,
          rawData: event,
          columns,
          groupKey: `res:${event.method}:${event.path}:${event.status}`,
        }
      }
      case EventType.IncomingRequestBody: {
        const size = `${formatBytes(event.bytes)}${event.truncated ? ' · truncated' : ''}`
        const columns: EventColumns = {
          method: event.method,
          path: `${event.path} · request body (${size})`,
          status: '',
          statusTone: 'muted',
        }
        return {
          type: EventType.IncomingRequest,
          summary: `${event.method} ${event.path} request body (${formatBytes(event.bytes)}${event.truncated ? ', truncated' : ''})`,
          rawData: decodeBody(event),
          columns,
          groupKey: `reqbody:${event.method}:${event.path}`,
        }
      }
      case EventType.IncomingResponseBody: {
        const size = `${formatBytes(event.bytes)}${event.truncated ? ' · truncated' : ''}`
        const columns: EventColumns = {
          method: event.method,
          path: `${event.path} · response body (${size})`,
          status: String(event.status),
          statusTone: httpStatusTone(event.status),
        }
        return {
          type: EventType.IncomingRequest,
          summary: `${event.method} ${event.path} → ${event.status} body (${formatBytes(event.bytes)}${event.truncated ? ', truncated' : ''})`,
          rawData: decodeBody(event),
          columns,
          groupKey: `resbody:${event.method}:${event.path}:${event.status}`,
        }
      }
      case EventType.OutgoingConnection: {
        const columns: EventColumns = {
          method: 'CONN',
          path: `${event.address}:${event.port}`,
          status: '',
          statusTone: 'muted',
        }
        return {
          type: EventType.OutgoingConnection,
          summary: `Outgoing: ${event.address}:${event.port}`,
          columns,
          groupKey: `out:${event.address}:${event.port}`,
        }
      }
      case EventType.PortSubscription:
      case EventType.EnvVar:
        return null
      case EventType.LayerConnected: {
        const columns: EventColumns = {
          method: 'PROC',
          path: `${event.process_name} (PID ${event.pid})`,
          status: 'UP',
          statusTone: 'ok',
        }
        return {
          type: EventType.LayerConnected,
          summary: `Process connected: ${event.process_name} (PID ${event.pid})`,
          columns,
          groupKey: `proc-up:${event.pid}`,
        }
      }
      case EventType.LayerDisconnected: {
        const columns: EventColumns = {
          method: 'PROC',
          path: `PID ${event.pid}`,
          status: 'DOWN',
          statusTone: 'muted',
        }
        return {
          type: EventType.LayerDisconnected,
          summary: `Process disconnected (PID ${event.pid})`,
          columns,
          groupKey: `proc-down:${event.pid}`,
        }
      }
      default:
        return null
    }
  } catch (err) {
    console.warn('Failed to parse event', event, err)
    return null
  }
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

export function formatTime24(date: Date): string {
  // Pass undefined locale so the browser uses the user's default locale.
  return date.toLocaleTimeString(undefined, { hour12: false })
}
