import { cn } from '@metalbear/ui'

export default function LiveDot({ active, className }: { active: boolean; className?: string }) {
  return (
    <span
      className={cn(
        'inline-flex rounded-full h-2 w-2',
        active ? 'bg-green-500' : 'bg-muted-foreground/30',
        className
      )}
    />
  )
}
