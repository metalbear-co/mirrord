import { Card, CardContent, CardHeader, cn } from '@metalbear/ui'
import { ChevronDown, ChevronRight } from 'lucide-react'
import { useState } from 'react'

interface WidgetProps {
  title?: string
  icon?: React.ReactNode
  trailing?: React.ReactNode
  children: React.ReactNode
  className?: string
  bodyClassName?: string
  collapsible?: boolean
  defaultOpen?: boolean
  noPadding?: boolean
}

export default function Widget({
  title,
  icon,
  trailing,
  children,
  className,
  bodyClassName,
  collapsible = false,
  defaultOpen = true,
  noPadding = false,
}: WidgetProps) {
  const [open, setOpen] = useState(defaultOpen)

  return (
    <Card
      className={cn('flex min-h-0 flex-col overflow-hidden p-0', className)}
    >
      {(title || trailing) && (
        <CardHeader
          className={cn(
            'surface-section border-border flex shrink-0 flex-row items-center gap-2 border-b px-3 py-2',
            collapsible && 'hover:bg-card/70 cursor-pointer',
          )}
          onClick={collapsible ? () => setOpen((v) => !v) : undefined}
        >
          {collapsible && (
            <span className="text-muted-foreground">
              {open ? (
                <ChevronDown className="h-3 w-3" />
              ) : (
                <ChevronRight className="h-3 w-3" />
              )}
            </span>
          )}
          {icon && <span className="text-muted-foreground">{icon}</span>}
          <span className="text-section text-foreground flex-1">{title}</span>
          {trailing && <span className="shrink-0">{trailing}</span>}
        </CardHeader>
      )}
      {open && (
        <CardContent
          className={cn(
            'min-h-0 flex-1 overflow-auto',
            noPadding ? 'p-0' : 'p-0',
            bodyClassName,
          )}
        >
          {children}
        </CardContent>
      )}
    </Card>
  )
}
