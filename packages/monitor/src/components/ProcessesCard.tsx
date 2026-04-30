import { Card, CardContent, CardHeader } from '@metalbear/ui'
import type { ProcessInfo } from '../types'
import { strings } from '../strings'

export default function ProcessesCard({ processes }: { processes: ProcessInfo[] }) {
  if (processes.length === 0) return null
  return (
    <Card className="overflow-hidden p-0">
      <CardHeader className="px-4 py-2.5 bg-card/50 border-b border-border">
        <span className="text-[11px] font-semibold text-foreground uppercase tracking-wider">
          {strings.session.sectionProcesses}
        </span>
      </CardHeader>
      <CardContent className="p-0 divide-y divide-border">
        {processes.map((p) => (
          <div
            key={p.pid}
            className="grid grid-cols-[1fr_max-content] items-baseline gap-3 px-4 py-1.5"
          >
            <span className="text-xs font-mono font-medium text-foreground truncate">
              {p.process_name || strings.session.unknownProcess}
            </span>
            <span className="text-xs font-mono text-muted-foreground tabular-nums">
              {p.pid}
            </span>
          </div>
        ))}
      </CardContent>
    </Card>
  )
}
