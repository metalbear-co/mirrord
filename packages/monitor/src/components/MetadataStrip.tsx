import { Card } from '@metalbear/ui'
import type { ReactNode } from 'react'

import { NOT_SET } from '../utils'

interface MetadataStripProps {
  items: { label: string; value: ReactNode }[]
}

// Both session views hand this the same field list in the same order, so a field that does have a
// value always appears under the same label in the same place. Dropping the ones with nothing to
// say is what keeps that consistency from turning into a row of placeholders: a typical session
// has no container and no queue splits, and printing a dash for each says nothing the absent field
// doesn't already say.
function hasSomethingToSay(item: { value: ReactNode }) {
  return item.value !== NOT_SET
}

export default function MetadataStrip({ items }: MetadataStripProps) {
  const shown = items.filter(hasSomethingToSay)
  if (shown.length === 0) return null
  return (
    <Card className="overflow-hidden p-0">
      <div className="divide-border flex flex-wrap divide-x">
        {shown.map((it) => (
          <div
            key={it.label}
            // `basis` is what makes the row wrap rather than squeeze: without a floor, `flex-1`
            // shrinks every cell until values like a session id are truncated to nothing.
            className="flex min-w-0 flex-1 basis-44 flex-col gap-0.5 px-4 py-2"
          >
            <span className="text-caps text-muted-foreground">{it.label}</span>
            <span
              className="text-body text-foreground font-mono break-words"
              title={typeof it.value === 'string' ? it.value : undefined}
            >
              {it.value}
            </span>
          </div>
        ))}
      </div>
    </Card>
  )
}
