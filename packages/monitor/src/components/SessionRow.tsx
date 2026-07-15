import { cn } from '@metalbear/ui'
import { ChevronRight } from 'lucide-react'
import type { ReactNode, MouseEvent } from 'react'

interface SessionRowProps {
  lead: ReactNode
  target: string
  meta?: ReactNode[]
  tags?: ReactNode[]
  action?: ReactNode
  right?: ReactNode
  selected?: boolean
  onClick?: () => void
  leftStrip?: string | undefined
}

export default function SessionRow({
  lead,
  target,
  meta = [],
  tags = [],
  action,
  right,
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
        'group relative flex min-h-[54px] w-full items-center gap-2.5 rounded-lg border px-3 py-2.5 text-left transition-colors',
        selected
          ? 'border-primary bg-primary/15'
          : 'border-border bg-card hover:border-muted-foreground',
      )}
    >
      {leftStrip && (
        <span
          className="absolute bottom-2 left-0 top-2 w-[3px] rounded-r-sm"
          style={{ background: leftStrip }}
        />
      )}
      <div className="flex h-[26px] w-[26px] shrink-0 items-center justify-center">{lead}</div>
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <div className="flex min-w-0 items-center gap-1.5">
          <span className="text-body text-foreground truncate font-mono font-semibold">
            {target}
          </span>
          {tags.map((t, i) => (
            <span key={i} className="shrink-0">
              {t}
            </span>
          ))}
        </div>
        {meta.length > 0 && (
          <div className="text-meta text-muted-foreground flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-0.5">
            {meta.map((m, i) => (
              <span key={i} className="inline-flex items-center gap-1.5 whitespace-nowrap">
                {i > 0 && <span className="opacity-50">·</span>}
                {m}
              </span>
            ))}
          </div>
        )}
      </div>
      {right && <div className="flex shrink-0 items-center">{right}</div>}
      <div
        role="presentation"
        className="flex shrink-0 items-center gap-1.5"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => e.stopPropagation()}
      >
        {action ?? <ChevronRight className="text-muted-foreground h-3.5 w-3.5" />}
      </div>
    </button>
  )
}
