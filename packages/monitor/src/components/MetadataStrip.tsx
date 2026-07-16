import { Card } from '@metalbear/ui'
import type { ReactNode } from 'react'

interface MetadataStripProps {
  items: { label: string; value: ReactNode }[]
}

export default function MetadataStrip({ items }: MetadataStripProps) {
  if (items.length === 0) return null
  return (
    <Card className="overflow-hidden p-0">
      <div className="divide-border flex flex-wrap divide-x">
        {items.map((it) => (
          <div
            key={it.label}
            className="flex min-w-0 flex-1 flex-col gap-0.5 px-4 py-2"
          >
            <span className="text-caps text-muted-foreground">{it.label}</span>
            <span className="text-body text-foreground truncate font-mono">
              {it.value}
            </span>
          </div>
        ))}
      </div>
    </Card>
  )
}
