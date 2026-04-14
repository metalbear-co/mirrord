import {
  Card,
  CardContent,
  CardHeader,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@metalbear/ui'
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
      <CardContent className="p-0">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{strings.session.columnName}</TableHead>
              <TableHead className="text-right">{strings.session.columnPid}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {processes.map((p) => (
              <TableRow key={p.pid}>
                <TableCell className="font-mono font-medium text-foreground">
                  {p.process_name || strings.session.unknownProcess}
                </TableCell>
                <TableCell className="font-mono text-muted-foreground text-right">{p.pid}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  )
}
