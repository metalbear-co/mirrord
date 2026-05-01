import { Card } from '@metalbear/ui'
import type { ReactNode } from 'react'

interface MetadataStripProps {
  items: { label: string; value: ReactNode }[]
}

export default function MetadataStrip({ items }: MetadataStripProps) {
  if (items.length === 0) return null
  return (
    <Card className="overflow-hidden p-0">
      <div className="flex flex-wrap divide-x divide-border">
        {items.map((it, i) => (
          <div
            key={i}
            className="flex flex-col gap-0.5 px-4 py-2 min-w-0 flex-1"
          >
            <span className="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
              {it.label}
            </span>
            <span className="text-xs font-mono text-foreground truncate">
              {it.value}
            </span>
          </div>
        ))}
      </div>
    </Card>
  )
}
