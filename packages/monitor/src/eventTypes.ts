export const EventType = {
  FileOp: 'file_op',
  DnsQuery: 'dns_query',
  IncomingRequest: 'incoming_request',
  OutgoingConnection: 'outgoing_connection',
  PortSubscription: 'port_subscription',
  EnvVar: 'env_var',
  LayerConnected: 'layer_connected',
  LayerDisconnected: 'layer_disconnected',
} as const

export type EventTypeValue = (typeof EventType)[keyof typeof EventType]
