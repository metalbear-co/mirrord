import {
  Code,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@metalbear/ui'

interface Props {
  detail: { summary: string; raw: string } | null
  onOpenChange: (open: boolean) => void
}

export default function EventDetailDialog({ detail, onOpenChange }: Props) {
  return (
    <Dialog open={!!detail} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[85vh] max-w-4xl">
        <DialogHeader>
          <DialogTitle className="text-foreground text-sm font-medium">
            {detail?.summary}
          </DialogTitle>
        </DialogHeader>
        <div className="relative max-h-[70vh] overflow-auto">
          <Code
            variant="block"
            className="whitespace-pre-wrap break-all text-xs"
          >
            {detail?.raw}
          </Code>
        </div>
      </DialogContent>
    </Dialog>
  )
}
