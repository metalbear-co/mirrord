import type { Activity } from 'lucide-react'

export type DetailTab = 'overview' | 'events' | 'config'

export interface EventCounts {
  incoming_request: number
  outgoing_connection: number
  dns_query: number
  file_op: number
  port_subscription: number
  env_var: number
  layer_connected: number
  layer_disconnected: number
  total: number
}

export const initialEventCounts: EventCounts = {
  incoming_request: 0,
  outgoing_connection: 0,
  dns_query: 0,
  file_op: 0,
  port_subscription: 0,
  env_var: 0,
  layer_connected: 0,
  layer_disconnected: 0,
  total: 0,
}

export interface TabDef {
  id: DetailTab
  label: string
  icon: typeof Activity
  count?: number
}
