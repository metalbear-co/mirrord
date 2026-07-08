import type { KeyboardEvent } from 'react'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@metalbear/ui'
import JsonHighlight from '../JsonHighlight'
import { strings } from '../../strings'

interface Props {
  detail: {
    summary: string
    raw: unknown
    position: { current: number; total: number }
  } | null
  onNavigate: (delta: number) => void
  onOpenChange: (open: boolean) => void
}

export default function EventDetailDialog({ detail, onNavigate, onOpenChange }: Props) {
  return (
    <Dialog open={!!detail} onOpenChange={onOpenChange}>
      <DialogContent
        className="max-w-4xl gap-0 p-0 overflow-hidden"
        onKeyDown={(e: KeyboardEvent<HTMLDivElement>) => {
          if (e.key === 'ArrowDown' || e.key === 'ArrowRight') {
            e.preventDefault()
            onNavigate(1)
          } else if (e.key === 'ArrowUp' || e.key === 'ArrowLeft') {
            e.preventDefault()
            onNavigate(-1)
          }
        }}
      >
        <DialogHeader className="border-b border-border surface-inset px-6 py-4 pr-12 space-y-1">
          <DialogTitle className="font-mono text-sm font-medium text-foreground break-all">
            {detail?.summary}
          </DialogTitle>
          <DialogDescription className="text-[11px] text-muted-foreground/60 tabular-nums">
            {detail
              ? `${detail.position.current} / ${detail.position.total} · ${strings.events.detailNavHint}`
              : ''}
          </DialogDescription>
        </DialogHeader>
        <div className="overflow-auto max-h-[70vh] px-6 py-4">
          <JsonHighlight value={detail?.raw} />
        </div>
      </DialogContent>
    </Dialog>
  )
}
