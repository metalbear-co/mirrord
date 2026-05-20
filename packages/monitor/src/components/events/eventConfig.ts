import { FileText, Globe, Zap, Activity, Server, Info } from 'lucide-react'
import { EventType, type EventTypeValue } from '../../eventTypes'
import { strings } from '../../strings'

// All event-type badges share one muted neutral treatment — the icon is the
// differentiator, the colour just adds noise. Mirrors Apple HIG's deference
// principle: the chrome stays out of the way and the content (the URL, path,
// or filename in each event row) is what the eye lands on.
const NEUTRAL_BADGE = 'border-border text-muted-foreground bg-muted/40'

export const EVENT_TYPE_CONFIG: Record<EventTypeValue, {
  variant: 'default' | 'destructive' | 'outline' | 'secondary'
  label: string
  className: string
  icon: typeof Activity
} | undefined> = {
  [EventType.FileOp]: { variant: 'outline', label: strings.events.labels.file, className: NEUTRAL_BADGE, icon: FileText },
  [EventType.DnsQuery]: { variant: 'outline', label: strings.events.labels.dns, className: NEUTRAL_BADGE, icon: Globe },
  [EventType.IncomingRequest]: { variant: 'outline', label: strings.events.labels.http, className: NEUTRAL_BADGE, icon: Zap },
  [EventType.OutgoingConnection]: { variant: 'outline', label: strings.events.labels.out, className: NEUTRAL_BADGE, icon: Server },
  [EventType.LayerConnected]: { variant: 'outline', label: strings.events.labels.info, className: NEUTRAL_BADGE, icon: Info },
  [EventType.LayerDisconnected]: { variant: 'outline', label: strings.events.labels.info, className: NEUTRAL_BADGE, icon: Info },
  [EventType.PortSubscription]: undefined,
  [EventType.EnvVar]: undefined,
}

const CHIP_INACTIVE = 'border-border text-muted-foreground hover:text-foreground hover:border-foreground/40'
const CHIP_ACTIVE = 'border-foreground/30 bg-muted text-foreground'

export const FILTER_CHIPS: { type: EventTypeValue | null; label: string; colorClass: string; activeClass: string }[] = [
  { type: null, label: strings.events.all, colorClass: CHIP_INACTIVE, activeClass: CHIP_ACTIVE },
  { type: EventType.IncomingRequest, label: strings.events.incoming, colorClass: CHIP_INACTIVE, activeClass: CHIP_ACTIVE },
  { type: EventType.DnsQuery, label: strings.events.dns, colorClass: CHIP_INACTIVE, activeClass: CHIP_ACTIVE },
  { type: EventType.FileOp, label: strings.events.fileOps, colorClass: CHIP_INACTIVE, activeClass: CHIP_ACTIVE },
  { type: EventType.OutgoingConnection, label: strings.events.outgoing, colorClass: CHIP_INACTIVE, activeClass: CHIP_ACTIVE },
]

export const MAX_EVENTS = 500
