import { cn } from '@metalbear/ui'

interface MiniToggleProps {
  on: boolean
  label: string
  onToggle: () => void
}

export default function MiniToggle({ on, label, onToggle }: MiniToggleProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      title={label}
      onClick={onToggle}
      className={cn(
        'focus-visible:ring-ring relative h-[15px] w-[26px] shrink-0 rounded-full transition-colors focus-visible:outline-none focus-visible:ring-1',
        on ? 'bg-chaos' : 'bg-border',
      )}
    >
      <span
        className={cn(
          'absolute top-0.5 h-[11px] w-[11px] rounded-full transition-all',
          on ? 'bg-card left-[13px]' : 'bg-muted-foreground left-0.5',
        )}
      />
    </button>
  )
}
