import { Code, Dialog, DialogContent, DialogHeader, DialogTitle } from '@metalbear/ui'

interface Props {
  detail: { summary: string; raw: string } | null
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
        <div className="relative overflow-auto max-h-[70vh]">
          <Code variant="block" className="text-xs whitespace-pre-wrap break-all">
            {detail?.raw}
          </Code>
        </div>
      </DialogContent>
    </Dialog>
  )
}
