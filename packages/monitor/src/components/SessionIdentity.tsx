import { Card, CardContent, CardHeader } from '@metalbear/ui'
import type { SessionInfo } from '../types'
import { strings } from '../strings'
import { extractLicenseKey } from '../utils'

export default function SessionIdentity({ session }: { session: SessionInfo }) {
  const licenseKey = extractLicenseKey(session.config)

  return (
    <Card className="overflow-hidden p-0">
      <CardHeader className="px-4 py-2.5 bg-card/50 border-b border-border">
        <span className="text-[11px] font-semibold text-foreground uppercase tracking-wider">
          {strings.session.sectionSession}
        </span>
      </CardHeader>
      <CardContent className="p-0 divide-y divide-border">
        <div className="grid grid-cols-[110px_1fr] items-baseline gap-3 px-4 py-1.5">
          <span className="text-xs text-muted-foreground">{strings.session.fieldSessionId}</span>
          <span className="text-xs font-mono text-foreground break-all">{session.session_id}</span>
        </div>
        {licenseKey && (
          <div className="grid grid-cols-[110px_1fr] items-baseline gap-3 px-4 py-1.5">
            <span className="text-xs text-muted-foreground">{strings.session.fieldKey}</span>
            <span className="text-xs font-mono text-foreground break-all">{licenseKey}</span>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
