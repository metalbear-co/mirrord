import { cn } from '@metalbear/ui'

export default function LiveDot({ active, className }: { active: boolean; className?: string }) {
  return (
    <span
      className={cn(
        'inline-flex h-2 w-2 rounded-full',
        active ? 'bg-green-500' : 'bg-muted-foreground/30',
        className,
      )}
    />
  )
}
