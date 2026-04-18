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
        <div className="flex items-center justify-between px-4 py-2.5">
          <span className="text-[10px] text-muted-foreground">{strings.session.fieldTarget}</span>
          <span className="text-xs font-mono font-medium text-foreground">{session.target}</span>
        </div>
        <div className="flex items-center justify-between px-4 py-2.5">
          <span className="text-[10px] text-muted-foreground">{strings.session.fieldSessionId}</span>
          <span className="text-xs font-mono text-foreground">{session.session_id}</span>
        </div>
        <div className="flex items-center justify-between px-4 py-2.5">
          <span className="text-[10px] text-muted-foreground">{strings.session.fieldVersion}</span>
          <span className="text-xs font-mono text-foreground">v{session.mirrord_version}</span>
        </div>
        {licenseKey && (
          <div className="flex items-center justify-between px-4 py-2.5">
            <span className="text-[10px] text-muted-foreground">{strings.session.fieldKey}</span>
            <span className="text-xs font-mono text-foreground">{licenseKey}</span>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
