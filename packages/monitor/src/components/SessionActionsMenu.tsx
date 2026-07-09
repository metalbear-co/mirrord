import { useEffect, useRef, useState } from 'react'
import { MoreVertical, SlidersHorizontal, Trash2 } from 'lucide-react'
import { Button } from '@metalbear/ui'
import { trackEvent } from '../analytics'
import { strings } from '../strings'

interface Props {
  onConfig: () => void
  onKill: () => void
}

// Overflow menu for per-session actions. Groups the benign Config with the destructive Kill so
// the header shows one quiet control instead of three competing buttons, and Kill is no longer a
// one-click mis-target next to Config.
export default function SessionActionsMenu({ onConfig, onKill }: Props) {
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const onClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false)
    }
    const onEsc = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false)
    }
    document.addEventListener('mousedown', onClick)
    document.addEventListener('keydown', onEsc)
    return () => {
      document.removeEventListener('mousedown', onClick)
      document.removeEventListener('keydown', onEsc)
    }
  }, [open])

  return (
    <div ref={ref} className="relative shrink-0">
      <Button
        variant="ghost"
        size="icon"
        className="h-6 w-6 text-muted-foreground hover:text-foreground"
        onClick={() => setOpen((o) => !o)}
        title={strings.session.actions}
        aria-label={strings.session.actions}
        aria-haspopup="menu"
        aria-expanded={open}
      >
        <MoreVertical className="h-3.5 w-3.5" />
      </Button>

      {open && (
        <div
          role="menu"
          className="absolute right-0 top-full mt-1.5 z-50 min-w-[180px] rounded-lg border border-border bg-popover text-popover-foreground shadow-lg p-1 flex flex-col"
        >
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setOpen(false)
              onConfig()
            }}
            className="flex items-center gap-2 px-2 py-1.5 rounded text-meta text-foreground hover:bg-muted transition-colors text-left"
          >
            <SlidersHorizontal className="h-3.5 w-3.5 text-muted-foreground" />
            {strings.session.config}
          </button>

          <div className="my-1 h-px bg-border" />

          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setOpen(false)
              trackEvent('session_monitor_kill_session')
              onKill()
            }}
            className="flex items-center gap-2 px-2 py-1.5 rounded text-meta text-destructive hover:bg-destructive/10 transition-colors text-left"
          >
            <Trash2 className="h-3.5 w-3.5" />
            {strings.session.kill}
          </button>
        </div>
      )}
    </div>
  )
}
