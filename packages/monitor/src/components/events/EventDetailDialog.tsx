import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@metalbear/ui'
import JsonHighlight from '../JsonHighlight'

interface Props {
  detail: { summary: string; raw: unknown } | null
  onOpenChange: (open: boolean) => void
}

export default function EventDetailDialog({ detail, onOpenChange }: Props) {
  return (
    <Dialog open={!!detail} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-4xl max-h-[85vh]">
        <DialogHeader>
          <DialogTitle className="text-sm font-medium text-foreground">
            {detail?.summary}
          </DialogTitle>
        </DialogHeader>
        <div className="relative overflow-auto max-h-[70vh] rounded-lg surface-inset p-4">
          <JsonHighlight value={detail?.raw} />
        </div>
      </DialogContent>
    </Dialog>
  )
}
