import { cn } from '@metalbear/ui'

export default function LiveDot({ active, className }: { active: boolean; className?: string }) {
  return (
    <span className={cn('relative flex h-2 w-2', className)}>
      {active && (
        <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-primary opacity-75" />
      )}
      <span
        className={cn(
          'relative inline-flex rounded-full h-2 w-2',
          active ? 'bg-primary' : 'bg-muted-foreground/30'
        )}
      />
    </span>
  )
}
