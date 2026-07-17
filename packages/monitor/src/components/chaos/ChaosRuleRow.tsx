import {
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
import { Pencil, Timer, Trash2, Unplug } from 'lucide-react'
import type { ChaosRule } from '../../types'
import { strings } from '../../strings'

const ERROR_TYPE_LABEL: Record<string, string> = {
  reset: strings.chaos.errorTypeReset,
  timed_out: strings.chaos.errorTypeTimedOut,
  refused: strings.chaos.errorTypeRefused,
}

function effectCell(rule: ChaosRule): React.ReactNode {
  if (rule.selector.type !== 'tcp') return '—'
  const effect = rule.selector.effect

  if ('latency' in effect) {
    const { read_ms, write_ms, jitter_ms } = effect.latency
    const parts = [
      read_ms ? `read ${read_ms}ms` : null,
      write_ms ? `write ${write_ms}ms` : null,
      jitter_ms ? `±${jitter_ms}ms jitter` : null,
    ].filter(Boolean)
    return (
      <span className="flex flex-col gap-0.5">
        <span className="inline-flex items-center gap-1.5 font-medium text-foreground">
          <Timer className="h-3 w-3 text-muted-foreground" />
          {strings.chaos.effectLatency}
        </span>
        <span className="text-muted-foreground">{parts.join(' · ')}</span>
      </span>
    )
  }

  const { error_type, after_ms } = effect.connection_error
  const parts = [
    ERROR_TYPE_LABEL[error_type] ?? error_type,
    after_ms ? `after ${after_ms}ms` : null,
  ].filter(Boolean)
  return (
    <span className="flex flex-col gap-0.5">
      <span className="inline-flex items-center gap-1.5 font-medium text-foreground">
        <Unplug className="h-3 w-3 text-muted-foreground" />
        {strings.chaos.effectConnectionError}
      </span>
      <span className="text-muted-foreground">{parts.join(' · ')}</span>
    </span>
  )
}

interface ChaosRuleRowProps {
  rule: ChaosRule
  onEdit: () => void
  onDelete: () => void
}

export default function ChaosRuleRow({
  rule,
  onEdit,
  onDelete,
}: ChaosRuleRowProps) {
  const upstream = rule.selector.type === 'tcp' ? rule.selector.upstream : '—'
  const percentage =
    rule.selector.type === 'tcp' ? `${rule.selector.percentage}%` : '—'

  return (
    <TableRow>
      <TableCell className="font-medium text-foreground">
        {rule.name ?? (
          <span className="text-muted-foreground italic">
            {strings.chaos.unnamed}
          </span>
        )}
      </TableCell>
      <TableCell className="font-mono">{upstream}</TableCell>
      <TableCell>{effectCell(rule)}</TableCell>
      <TableCell className="text-right tabular-nums">{percentage}</TableCell>
      <TableCell className="text-right tabular-nums">{rule.priority}</TableCell>
      <TableCell
        className={
          rule.hit_count > 0
            ? 'text-right font-medium tabular-nums text-foreground'
            : 'text-right tabular-nums text-muted-foreground'
        }
      >
        {rule.hit_count}
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
                  {strings.chaos.deleteConfirmDescription(
                    rule.name ?? strings.chaos.unnamed,
                  )}
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
