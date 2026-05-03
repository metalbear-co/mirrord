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
      className={cn('overflow-hidden p-0 flex flex-col min-h-0', className)}
    >
      {title && (
        <CardHeader
          className={cn(
            'px-3 py-2 surface-section border-b border-border flex flex-row items-center gap-2 shrink-0',
            collapsible && 'cursor-pointer hover:bg-card/70'
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
            'flex-1 min-h-0 overflow-auto',
            noPadding ? 'p-0' : 'p-0',
            bodyClassName
          )}
        >
          {children}
        </CardContent>
      )}
    </Card>
  )
}
