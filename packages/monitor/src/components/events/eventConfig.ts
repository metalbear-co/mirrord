import { FileText, Globe, Zap, Activity, Server, Info } from 'lucide-react'
import { EventType, type EventTypeValue } from '../../eventTypes'
import { strings } from '../../strings'

// Per the session monitor redesign, event-type badges are tinted per kind so the type column
// scans at a glance in the table layout (HTTP in the brand tint, DNS amber, file red); the
// row content stays monospace and neutral.
export const EVENT_TYPE_CONFIG: Partial<Record<EventTypeValue, {
  variant: 'default' | 'destructive' | 'outline' | 'secondary'
  label: string
  className: string
  icon: typeof Activity
}>> = {
  [EventType.FileOp]: { variant: 'outline', label: strings.events.labels.file, className: 'border-transparent bg-red-100 text-red-700 dark:bg-red-950 dark:text-red-400', icon: FileText },
  [EventType.DnsQuery]: { variant: 'outline', label: strings.events.labels.dns, className: 'border-transparent bg-amber-100 text-amber-800 dark:bg-amber-950 dark:text-amber-400', icon: Globe },
  [EventType.IncomingRequest]: { variant: 'outline', label: strings.events.labels.http, className: 'border-transparent bg-primary/15 text-foreground', icon: Zap },
  [EventType.OutgoingConnection]: { variant: 'outline', label: strings.events.labels.out, className: 'border-transparent bg-sky-100 text-sky-800 dark:bg-sky-950 dark:text-sky-400', icon: Server },
  [EventType.LayerConnected]: { variant: 'outline', label: strings.events.labels.info, className: 'border-border text-muted-foreground bg-muted/40', icon: Info },
  [EventType.LayerDisconnected]: { variant: 'outline', label: strings.events.labels.info, className: 'border-border text-muted-foreground bg-muted/40', icon: Info },
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
