import { useEffect, useRef, useState } from 'react'
import { cn } from '@metalbear/ui'
import Kbd from './Kbd'

export interface Command {
  id: string
  label: string
  hint?: string
  // Shortcut keycaps shown on the right, teaching the direct binding.
  keys?: string[]
  run: () => void
}

interface Props {
  open: boolean
  commands: Command[]
  onClose: () => void
}

// ⌘K palette: one keystroke reveals every action in the view with its direct shortcut, so the
// palette doubles as the shortcut cheat-sheet. Actions run and close; the list also teaches the
// faster direct keys next to each entry.
export default function CommandPalette({ open, commands, onClose }: Props) {
  const [query, setQuery] = useState('')
  const [active, setActive] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (open) {
      setQuery('')
      setActive(0)
      // Focus after paint so the keydown that opened the palette doesn't land in the input.
      const id = requestAnimationFrame(() => inputRef.current?.focus())
      return () => cancelAnimationFrame(id)
    }
  }, [open])

  if (!open) return null

  const filtered = commands.filter((c) => c.label.toLowerCase().includes(query.toLowerCase()))
  const clampedActive = Math.min(active, Math.max(0, filtered.length - 1))

  const runAt = (index: number) => {
    const command = filtered[index]
    if (command) {
      onClose()
      command.run()
    }
  }

  return (
    <div className="fixed inset-0 z-[60] flex items-start justify-center pt-[15vh]" onClick={onClose}>
      <div className="absolute inset-0 bg-black/40" />
      <div
        className="relative w-[520px] max-w-[90vw] bg-card border border-border rounded-xl shadow-2xl overflow-hidden"
        onClick={(e) => e.stopPropagation()}
      >
        <input
          ref={inputRef}
          value={query}
          onChange={(e) => {
            setQuery(e.target.value)
            setActive(0)
          }}
          onKeyDown={(e) => {
            if (e.key === 'ArrowDown') {
              e.preventDefault()
              setActive((a) => Math.min(a + 1, filtered.length - 1))
            } else if (e.key === 'ArrowUp') {
              e.preventDefault()
              setActive((a) => Math.max(a - 1, 0))
            } else if (e.key === 'Enter') {
              e.preventDefault()
              runAt(clampedActive)
            } else if (e.key === 'Escape') {
              e.preventDefault()
              onClose()
            }
          }}
          placeholder="Type a command…"
          className="w-full px-4 py-3 bg-transparent text-sm outline-none border-b border-border placeholder:text-muted-foreground"
        />
        <div className="max-h-[320px] overflow-y-auto py-1">
          {filtered.length === 0 && (
            <div className="px-4 py-6 text-center text-meta text-muted-foreground">
              No matching commands.
            </div>
          )}
          {filtered.map((command, i) => (
            <button
              key={command.id}
              onMouseEnter={() => setActive(i)}
              onClick={() => runAt(i)}
              className={cn(
                'w-full flex items-center gap-3 px-4 py-2 text-left text-sm transition-colors',
                i === clampedActive ? 'bg-primary/10' : 'hover:bg-muted/40',
              )}
            >
              <span className="flex-1 min-w-0 truncate">{command.label}</span>
              {command.hint && (
                <span className="text-meta text-muted-foreground truncate">{command.hint}</span>
              )}
              {command.keys && (
                <span className="inline-flex items-center gap-1 shrink-0">
                  {command.keys.map((k) => (
                    <Kbd key={k}>{k}</Kbd>
                  ))}
                </span>
              )}
            </button>
          ))}
        </div>
        <div className="flex items-center gap-3 px-4 py-2 border-t border-border text-[11px] text-muted-foreground">
          <span className="inline-flex items-center gap-1">
            <Kbd>↑</Kbd>
            <Kbd>↓</Kbd>
            navigate
          </span>
          <span className="inline-flex items-center gap-1">
            <Kbd>↵</Kbd>
            run
          </span>
          <span className="inline-flex items-center gap-1">
            <Kbd>Esc</Kbd>
            close
          </span>
        </div>
      </div>
    </div>
  )
}
