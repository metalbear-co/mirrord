import { useState } from 'react'
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Textarea,
} from '@metalbear/ui'
import { MessageSquarePlus } from 'lucide-react'
import { trackEvent } from '../../analytics'
import { strings } from '../../strings'

/// Lets users tell us what chaos selector/effect they actually want us to support.
export default function RequestChaosTypeDialog() {
  const s = strings.chaos
  const [open, setOpen] = useState(false)
  const [text, setText] = useState('')
  const [sent, setSent] = useState(false)

  function handleOpenChange(next: boolean) {
    setOpen(next)
    if (!next) {
      setText('')
      setSent(false)
    }
  }

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault()
    const request = text.trim()
    if (!request) return
    trackEvent('chaos_type_requested', { request })
    setSent(true)
  }

  return (
    <>
      <Button
        size="sm"
        className="shrink-0 gap-1.5"
        onClick={() => setOpen(true)}
      >
        <MessageSquarePlus className="h-3.5 w-3.5" />
        {s.requestTypeButton}
      </Button>
      <Dialog open={open} onOpenChange={handleOpenChange}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{s.requestTypeDialogTitle}</DialogTitle>
            <DialogDescription>
              {s.requestTypeDialogDescription}
            </DialogDescription>
          </DialogHeader>

          {sent ? (
            <p className="text-meta text-foreground">{s.requestTypeSent}</p>
          ) : (
            <form onSubmit={handleSubmit} className="flex flex-col gap-3">
              <Textarea
                rows={3}
                value={text}
                placeholder={s.requestTypePlaceholder}
                onChange={(e) => setText(e.target.value)}
              />
              <DialogFooter>
                <Button
                  type="button"
                  variant="ghost"
                  onClick={() => handleOpenChange(false)}
                >
                  {s.cancel}
                </Button>
                <Button type="submit" disabled={!text.trim()}>
                  {s.requestTypeSubmit}
                </Button>
              </DialogFooter>
            </form>
          )}
        </DialogContent>
      </Dialog>
    </>
  )
}
