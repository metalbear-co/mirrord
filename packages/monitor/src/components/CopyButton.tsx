import { useState } from 'react'
import { Button } from '@metalbear/ui'
import { Check, Copy } from 'lucide-react'

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
          navigator.clipboard.writeText(getText())
          setCopied(true)
          setTimeout(() => setCopied(false), 1200)
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
