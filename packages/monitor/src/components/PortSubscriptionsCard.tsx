import { Badge, Card, CardContent, CardHeader } from '@metalbear/ui'
import type { PortSubscription } from '../types'
import { strings } from '../strings'

export default function PortSubscriptionsCard({ portSubs }: { portSubs: PortSubscription[] }) {
  return (
    <Card className="overflow-hidden p-0">
      <CardHeader className="px-4 py-2.5 bg-card/50 border-b border-border">
        <span className="text-[11px] font-semibold text-foreground uppercase tracking-wider">
          {strings.session.sectionPorts}
        </span>
      </CardHeader>
      <CardContent className="p-0">
        {portSubs.length > 0 ? (
          <div className="divide-y divide-border">
            {portSubs.map((p) => (
              <div key={p.port} className="flex items-center justify-between px-4 py-2.5">
                <span className="text-xs font-mono font-medium text-foreground">:{p.port}</span>
                <Badge variant="outline" className="text-[9px] px-1.5 py-0 h-4 font-mono">
                  {p.mode}
                </Badge>
              </div>
            ))}
          </div>
        ) : (
          <div className="px-4 py-3 text-xs text-muted-foreground">{strings.session.noPorts}</div>
        )}
      </CardContent>
    </Card>
  )
}
