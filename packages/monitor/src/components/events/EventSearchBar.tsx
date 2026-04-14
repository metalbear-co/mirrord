import { useRef, useState } from 'react'
import { Button } from '@metalbear/ui'
import { Search, X } from 'lucide-react'
import { strings } from '../../strings'

interface Props {
  query: string
  onChange: (query: string) => void
}

export default function EventSearchBar({ query, onChange }: Props) {
  const [open, setOpen] = useState(false)
  const inputRef = useRef<HTMLInputElement>(null)

  if (!open) {
    return (
      <Button
        variant="ghost"
        size="icon"
        onClick={() => {
          setOpen(true)
          setTimeout(() => inputRef.current?.focus(), 100)
        }}
        title={strings.events.search}
        className="h-6 w-6"
      >
        <Search className="h-3.5 w-3.5" />
      </Button>
    )
  }

  return (
    <div className="flex items-center gap-2 flex-1">
      <Search className="h-3.5 w-3.5 text-muted-foreground shrink-0" />
      <input
        ref={inputRef}
        type="text"
        value={query}
        onChange={(e) => onChange(e.target.value)}
        placeholder={strings.events.searchPlaceholder}
        className="flex-1 bg-transparent text-xs text-foreground placeholder:text-muted-foreground/50 outline-none border border-border rounded px-2 py-1 focus:border-primary"
      />
      <Button
        variant="ghost"
        size="icon"
        onClick={() => {
          setOpen(false)
          onChange('')
        }}
        className="h-6 w-6"
      >
        <X className="h-3.5 w-3.5" />
      </Button>
    </div>
  )
}
