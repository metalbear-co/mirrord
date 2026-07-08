import type { ReactNode } from 'react'

interface MetadataStripProps {
  items: { label: string; value: ReactNode }[]
  trailing?: ReactNode
}

export default function MetadataStrip({ items, trailing }: MetadataStripProps) {
  if (items.length === 0 && !trailing) return null
  return (
    <div className="flex flex-wrap items-center gap-2">
      {items.map((it, i) => (
        <div
          key={i}
          className="inline-flex items-center gap-1.5 border border-border bg-card rounded-full px-3 py-1 min-w-0"
        >
          <span className="text-[10px] font-semibold tracking-wider uppercase text-muted-foreground whitespace-nowrap">
            {it.label}
          </span>
          <span className="text-xs font-mono text-foreground truncate">{it.value}</span>
        </div>
      ))}
      {trailing}
    </div>
  )
}
