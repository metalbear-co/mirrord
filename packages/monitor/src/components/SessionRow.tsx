import { cn } from '@metalbear/ui'
import { ChevronRight } from 'lucide-react'
import type { ReactNode, MouseEvent } from 'react'

interface SessionRowProps {
  lead: ReactNode
  target: string
  meta?: ReactNode[]
  tags?: ReactNode[]
  action?: ReactNode
  selected?: boolean
  onClick?: () => void
  leftStrip?: string
}

export default function SessionRow({
  lead,
  target,
  meta = [],
  tags = [],
  action,
  selected = false,
  onClick,
  leftStrip,
}: SessionRowProps) {
  const handleClick = (e: MouseEvent) => {
    e.preventDefault()
    onClick?.()
  }
  return (
    <button
      type="button"
      onClick={handleClick}
      className={cn(
        'group relative w-full text-left flex items-center gap-2.5 px-3 py-2.5 rounded-lg border transition-colors min-h-[54px]',
        selected
          ? 'border-primary bg-primary/15'
          : 'border-border bg-card hover:border-muted-foreground'
      )}
    >
      {leftStrip && (
        <span
          className="absolute left-0 top-2 bottom-2 w-[3px] rounded-r-sm"
          style={{ background: leftStrip }}
        />
      )}
      <div className="flex items-center justify-center w-[26px] h-[26px] shrink-0">
        {lead}
      </div>
      <div className="flex-1 min-w-0 flex flex-col gap-1">
        <div className="flex items-center gap-1.5 min-w-0">
          <span className="font-mono text-body font-semibold text-foreground truncate">
            {target}
          </span>
          {tags.map((t, i) => (
            <span key={i} className="shrink-0">
              {t}
            </span>
          ))}
        </div>
        {meta.length > 0 && (
          <div className="flex items-center gap-1.5 text-meta text-muted-foreground min-w-0">
            {meta.map((m, i) => (
              <span key={i} className="flex items-center gap-1.5 truncate">
                {i > 0 && <span className="opacity-50">·</span>}
                <span className="truncate">{m}</span>
              </span>
            ))}
          </div>
        )}
      </div>
      <div
        className="shrink-0 flex items-center gap-1.5"
        onClick={(e) => e.stopPropagation()}
      >
        {action ?? (
          <ChevronRight className="h-3.5 w-3.5 text-muted-foreground" />
        )}
      </div>
    </button>
  )
}
