import { useEffect, useState } from 'react'
import { Button } from '@metalbear/ui'
import { X } from 'lucide-react'
import type { ChaosEffectKind } from '../../types'
import type { ChaosRuleFields } from '../../hooks/useChaosRules'
import { strings } from '../../strings'

const DEFAULT_READ_MS = '800'
const DEFAULT_PCT_NUM = 50
const DEFAULT_PCT = String(DEFAULT_PCT_NUM)

const POPOVER_EFFECTS: { kind: ChaosEffectKind; label: string }[] = [
  { kind: 'latency', label: strings.chaos.popoverAddLatency },
  { kind: 'reset', label: strings.chaos.popoverReset },
  { kind: 'refused', label: strings.chaos.popoverRefuse },
]

function clampPct(value: string): number {
  const n = parseInt(value, 10)
  if (!Number.isFinite(n)) return DEFAULT_PCT_NUM
  return Math.min(100, Math.max(0, n))
}

interface BreakPopoverProps {
  host: string
  onClose: () => void
  onArm: (fields: ChaosRuleFields) => Promise<void>
  onMore: (host: string) => void
}

export default function BreakPopover({
  host,
  onClose,
  onArm,
  onMore,
}: BreakPopoverProps) {
  const s = strings.chaos
  const [effect, setEffect] = useState<ChaosEffectKind>('latency')
  const [ms, setMs] = useState(DEFAULT_READ_MS)
  const [pct, setPct] = useState(DEFAULT_PCT)
  const [arming, setArming] = useState(false)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [onClose])

  async function arm() {
    setArming(true)
    setError(null)
    try {
      await onArm({
        name: '',
        upstream: host,
        effectKind: effect,
        readMs: effect === 'latency' ? parseInt(ms, 10) || 0 : 0,
        writeMs: 0,
        jitterMs: 0,
        afterMs: 0,
        percentage: clampPct(pct),
        priority: 0,
      })
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setArming(false)
    }
  }

  return (
    <div className="bg-card border-primary absolute right-3 top-11 z-10 w-80 rounded-lg border p-3.5 shadow-xl">
      <div className="mb-0.5 flex items-start gap-2">
        <div className="text-section text-foreground min-w-0">
          {s.popoverTitle} <span className="break-all font-mono">{host}</span>
        </div>
        <Button
          variant="ghost"
          size="icon"
          className="ml-auto h-5 w-5 shrink-0"
          onClick={onClose}
          aria-label={s.cancel}
        >
          <X className="h-3.5 w-3.5" />
        </Button>
      </div>
      <div className="text-meta text-muted-foreground mb-2.5">
        {s.popoverScope}
      </div>

      <div className="mb-2.5 flex gap-1.5">
        {POPOVER_EFFECTS.map((option) => (
          <button
            key={option.kind}
            type="button"
            onClick={() => setEffect(option.kind)}
            className={
              'text-meta flex-1 rounded-md border py-1.5 text-center font-medium transition-colors ' +
              (effect === option.kind
                ? 'border-primary bg-primary/25 text-foreground'
                : 'border-border text-muted-foreground hover:text-foreground')
            }
          >
            {option.label}
          </button>
        ))}
      </div>

      <div className="mb-3 flex gap-1.5">
        {effect === 'latency' && (
          <label className="bg-muted/30 border-border flex flex-1 items-center gap-1.5 rounded-md border px-2 py-1">
            <input
              value={ms}
              onChange={(e) => setMs(e.target.value)}
              className="text-body text-foreground w-11 min-w-0 bg-transparent font-mono outline-none"
              inputMode="numeric"
            />
            <span className="text-meta text-muted-foreground">
              {s.unitMsRead}
            </span>
          </label>
        )}
        <label className="bg-muted/30 border-border flex flex-1 items-center gap-1.5 rounded-md border px-2 py-1">
          <input
            value={pct}
            onChange={(e) => setPct(e.target.value)}
            className="text-body text-foreground w-9 min-w-0 bg-transparent font-mono outline-none"
            inputMode="numeric"
          />
          <span className="text-meta text-muted-foreground">
            {s.unitPctTraffic}
          </span>
        </label>
      </div>

      {error && (
        <p className="text-meta text-destructive mb-2 break-words">{error}</p>
      )}

      <div className="flex items-center justify-end gap-3">
        <button
          type="button"
          onClick={() => onMore(host)}
          className="text-meta text-primary hover:underline"
        >
          {s.popoverMore}
        </button>
        <Button
          size="sm"
          variant="outline"
          className="h-7"
          disabled={arming}
          onClick={() => void arm()}
        >
          {s.popoverArm}
        </Button>
      </div>
    </div>
  )
}
