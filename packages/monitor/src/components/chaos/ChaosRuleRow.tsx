import {
  Badge,
  Button,
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
  TableCell,
  TableRow,
} from '@metalbear/ui'
import { Pencil, Trash2 } from 'lucide-react'
import type { ChaosRule } from '../../types'
import { strings } from '../../strings'

const ERROR_TYPE_LABEL: Record<string, string> = {
  reset: strings.chaos.errorTypeReset,
  timed_out: strings.chaos.errorTypeTimedOut,
  refused: strings.chaos.errorTypeRefused,
}

function effectSummary(rule: ChaosRule): string {
  if (rule.selector.type !== 'tcp') return '—'
  const effect = rule.selector.effect

  if ('latency' in effect) {
    const { read_ms, write_ms, jitter_ms } = effect.latency
    const parts = [
      read_ms ? `read ${read_ms}ms` : null,
      write_ms ? `write ${write_ms}ms` : null,
    ].filter(Boolean)
    const jitter = jitter_ms ? ` (±${jitter_ms}ms jitter)` : ''
    return `${strings.chaos.effectLatency}: ${parts.join(', ')}${jitter}`
  }

  const { error_type, after_ms } = effect.connection_error
  const after = after_ms ? ` after ${after_ms}ms` : ''
  return `${strings.chaos.effectConnectionError}: ${ERROR_TYPE_LABEL[error_type] ?? error_type}${after}`
}

interface ChaosRuleRowProps {
  rule: ChaosRule
  onEdit: () => void
  onDelete: () => void
}

export default function ChaosRuleRow({ rule, onEdit, onDelete }: ChaosRuleRowProps) {
  const upstream = rule.selector.type === 'tcp' ? rule.selector.upstream : '—'
  const percentage = rule.selector.type === 'tcp' ? `${rule.selector.percentage}%` : '—'

  return (
    <TableRow>
      <TableCell className="font-medium text-foreground">
        {rule.name || <span className="text-muted-foreground italic">{strings.chaos.unnamed}</span>}
      </TableCell>
      <TableCell className="font-mono">{upstream}</TableCell>
      <TableCell>{effectSummary(rule)}</TableCell>
      <TableCell>{percentage}</TableCell>
      <TableCell>{rule.priority}</TableCell>
      <TableCell>
        <Badge variant="secondary">{rule.hit_count}</Badge>
      </TableCell>
      <TableCell className="text-right">
        <div className="flex justify-end gap-1">
          <Button
            variant="ghost"
            size="icon"
            title={strings.chaos.edit}
            aria-label={strings.chaos.edit}
            className="h-7 w-7"
            onClick={onEdit}
          >
            <Pencil className="h-3.5 w-3.5" />
          </Button>
          <Dialog>
            <DialogTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                title={strings.chaos.delete}
                aria-label={strings.chaos.delete}
                className="h-7 w-7 text-muted-foreground hover:text-destructive hover:bg-destructive/10"
              >
                <Trash2 className="h-3.5 w-3.5" />
              </Button>
            </DialogTrigger>
            <DialogContent>
              <DialogHeader>
                <DialogTitle>{strings.chaos.deleteConfirmTitle}</DialogTitle>
                <DialogDescription>
                  {strings.chaos.deleteConfirmDescription(rule.name || strings.chaos.unnamed)}
                </DialogDescription>
              </DialogHeader>
              <DialogFooter>
                <DialogClose asChild>
                  <Button variant="outline" size="sm">
                    {strings.chaos.cancel}
                  </Button>
                </DialogClose>
                <DialogClose asChild>
                  <Button variant="destructive" size="sm" onClick={onDelete}>
                    <Trash2 className="h-3.5 w-3.5 mr-1.5" />
                    {strings.chaos.delete}
                  </Button>
                </DialogClose>
              </DialogFooter>
            </DialogContent>
          </Dialog>
        </div>
      </TableCell>
    </TableRow>
  )
}
