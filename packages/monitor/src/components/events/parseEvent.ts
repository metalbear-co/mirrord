import type { MonitorEvent } from '../../types'
import { EventType, type EventTypeValue } from '../../eventTypes'
import { strings } from '../../strings'

export interface ParsedEvent {
  type: EventTypeValue
  summary: string
  rawData?: string
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
        return { type: EventType.IncomingRequest, summary: `${event.method} ${event.host}${event.path}` }
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

export function formatTime24(date: Date): string {
  // Pass undefined locale so the browser uses the user's default locale.
  return date.toLocaleTimeString(undefined, { hour12: false })
}
