import { Badge, Card, CardContent, CardHeader } from '@metalbear/ui'
import type { PortSubscription } from '../types'
import { strings } from '../strings'

export default function PortSubscriptionsCard({ portSubs }: { portSubs: PortSubscription[] }) {
  return (
    <Card className="overflow-hidden p-0">
      <CardHeader className="px-4 py-2 surface-section border-b border-border">
        <span className="text-section text-foreground">
          {strings.session.sectionPorts}
        </span>
      </CardHeader>
      <CardContent className="p-0">
        {portSubs.length > 0 ? (
          <div className="divide-y divide-border">
            {portSubs.map((p) => (
              <div key={p.port} className="flex items-center justify-between px-4 py-1.5">
                <span className="text-body font-mono font-medium text-foreground">:{p.port}</span>
                <Badge variant="outline" className="text-meta px-2 py-0 font-mono font-normal">
                  {p.mode}
                </Badge>
              </div>
            ))}
          </div>
        ) : (
          <div className="px-4 py-3 text-body text-muted-foreground">{strings.session.noPorts}</div>
        )}
      </CardContent>
    </Card>
  )
}
