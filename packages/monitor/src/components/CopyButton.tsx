import { useState } from 'react'
import { Button } from '@metalbear/ui'
import { Check, Copy } from 'lucide-react'

const COPY_FEEDBACK_MS = 1200

interface CopyButtonProps {
  getText: () => string
  title?: string
}

export default function CopyButton({
  getText,
  title = 'Copy',
}: CopyButtonProps) {
  const [copied, setCopied] = useState(false)
  return (
    <Button
      variant="ghost"
      size="icon"
      onClick={(e) => {
        e.stopPropagation()
        try {
          void navigator.clipboard.writeText(getText())
          setCopied(true)
          setTimeout(() => setCopied(false), COPY_FEEDBACK_MS)
        } catch {}
      }}
      title={copied ? 'Copied' : title}
      aria-label={copied ? 'Copied' : title}
      className="h-6 w-6"
    >
      {copied ? (
        <Check className="h-3.5 w-3.5 text-emerald-500" />
      ) : (
        <Copy className="h-3.5 w-3.5" />
      )}
    </Button>
  )
}
