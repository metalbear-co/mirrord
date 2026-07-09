import type { ReactNode } from 'react'
import { cn } from '@metalbear/ui'

// Inline keycap used to teach shortcuts at their point of use (matching the search box's ⌘F
// hint). Shortcuts learned in context stick better than a shortcuts modal nobody opens.
export default function Kbd({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <kbd
      className={cn(
        'inline-flex items-center justify-center min-w-[16px] h-[16px] px-1',
        'text-[10px] font-sans font-medium leading-none',
        'border border-border rounded bg-card text-muted-foreground',
        className,
      )}
    >
      {children}
    </kbd>
  )
}
