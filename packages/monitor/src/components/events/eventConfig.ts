import { FileText, Globe, Zap, Activity, Server, Info } from 'lucide-react'
import { EventType, type EventTypeValue } from '../../eventTypes'
import { strings } from '../../strings'

// Badge colors use distinct per-category tokens (amber/blue/green/purple/sky) so
// event kinds remain visually distinguishable. The UI kit's semantic Badge variants
// only cover default/destructive/secondary/outline, which isn't enough categories.
export const EVENT_TYPE_CONFIG: Record<EventTypeValue, {
  variant: 'default' | 'destructive' | 'outline' | 'secondary'
  label: string
  className: string
  icon: typeof Activity
} | undefined> = {
  [EventType.FileOp]: { variant: 'outline', label: strings.events.labels.file, className: 'border-amber-500/50 text-amber-400 bg-amber-500/10', icon: FileText },
  [EventType.DnsQuery]: { variant: 'outline', label: strings.events.labels.dns, className: 'border-blue-500/50 text-blue-400 bg-blue-500/10', icon: Globe },
  [EventType.IncomingRequest]: { variant: 'outline', label: strings.events.labels.http, className: 'border-green-500/50 text-green-400 bg-green-500/10', icon: Zap },
  [EventType.OutgoingConnection]: { variant: 'outline', label: strings.events.labels.out, className: 'border-purple-500/50 text-purple-400 bg-purple-500/10', icon: Server },
  [EventType.LayerConnected]: { variant: 'outline', label: strings.events.labels.info, className: 'border-sky-500/50 text-sky-400 bg-sky-500/10', icon: Info },
  [EventType.LayerDisconnected]: { variant: 'outline', label: strings.events.labels.info, className: 'border-sky-500/50 text-sky-400 bg-sky-500/10', icon: Info },
  [EventType.PortSubscription]: undefined,
  [EventType.EnvVar]: undefined,
}

export const FILTER_CHIPS: { type: EventTypeValue | null; label: string; colorClass: string; activeClass: string }[] = [
  { type: null, label: strings.events.all, colorClass: 'border-primary/40 text-primary/70 hover:text-primary', activeClass: 'border-primary bg-primary/15 text-primary' },
  { type: EventType.IncomingRequest, label: strings.events.incoming, colorClass: 'border-green-500/30 text-green-500/60 hover:text-green-400', activeClass: 'border-green-500 bg-green-500/15 text-green-400' },
  { type: EventType.DnsQuery, label: strings.events.dns, colorClass: 'border-blue-500/30 text-blue-500/60 hover:text-blue-400', activeClass: 'border-blue-500 bg-blue-500/15 text-blue-400' },
  { type: EventType.FileOp, label: strings.events.fileOps, colorClass: 'border-amber-500/30 text-amber-500/60 hover:text-amber-400', activeClass: 'border-amber-500 bg-amber-500/15 text-amber-400' },
  { type: EventType.OutgoingConnection, label: strings.events.outgoing, colorClass: 'border-purple-500/30 text-purple-500/60 hover:text-purple-400', activeClass: 'border-purple-500 bg-purple-500/15 text-purple-400' },
]

export const MAX_EVENTS = 500
