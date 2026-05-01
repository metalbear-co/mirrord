import { cn } from '@metalbear/ui'

const PALETTE = [
  'bg-zinc-500/15 text-zinc-700 dark:text-zinc-300',
  'bg-stone-500/15 text-stone-700 dark:text-stone-300',
  'bg-neutral-500/15 text-neutral-700 dark:text-neutral-300',
  'bg-slate-500/15 text-slate-700 dark:text-slate-300',
]

function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean)
  if (parts.length === 0) return '?'
  if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase()
  return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase()
}

function paletteFor(seed: string): string {
  let h = 0
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) | 0
  return PALETTE[Math.abs(h) % PALETTE.length]
}

interface AvatarProps {
  name: string
  seed?: string
  size?: number
  ring?: boolean
}

export default function Avatar({ name, seed, size = 26, ring }: AvatarProps) {
  return (
    <span
      className={cn(
        'inline-flex items-center justify-center rounded-full font-semibold leading-none',
        paletteFor(seed ?? name),
        ring && 'ring-2 ring-primary ring-offset-1 ring-offset-card'
      )}
      style={{ width: size, height: size, fontSize: Math.max(10, size * 0.42) }}
    >
      {initials(name)}
    </span>
  )
}
