import { Button, cn } from '@metalbear/ui'
import { Pencil, Trash2 } from 'lucide-react'
import type { ClientChaosRule } from '../../types'
import { strings } from '../../strings'
import MiniToggle from './MiniToggle'
import { ruleDisplayName } from './chaosMatch'

const MIN_BAR_HEIGHT_PCT = 8

function effectSummary(rule: ClientChaosRule): string {
  const s = strings.chaos
  if (rule.effectKind === 'latency') {
    const parts = [
      s.latencyLabel,
      rule.readMs ? s.latencyRead(rule.readMs) : null,
      rule.writeMs ? s.latencyWrite(rule.writeMs) : null,
      rule.jitterMs ? s.latencyJitter(rule.jitterMs) : null,
    ].filter(Boolean)
    return parts.join(' ')
  }
  const label = {
    reset: s.errorReset,
    timed_out: s.errorTimedOut,
    refused: s.errorRefused,
  }[rule.effectKind]
  return rule.afterMs ? `${label} ${s.errorAfter(rule.afterMs)}` : label
}

interface ChaosRuleCardProps {
  rule: ClientChaosRule
  onToggle: () => void
  onEdit: () => void
  onDelete: () => void
}

export default function ChaosRuleCard({
  rule,
  onToggle,
  onEdit,
  onDelete,
}: ChaosRuleCardProps) {
  const s = strings.chaos
  const meta = [
    rule.upstream,
    effectSummary(rule),
    s.pctOf(rule.percentage),
    s.prioOf(rule.priority),
  ].join(s.metaSeparator)

  const sparkMax = Math.max(1, ...rule.spark.map((b) => b.value))

  return (
    <div
      className={cn(
        'border-border rounded-lg border bg-muted/30 px-3 py-2.5',
        !rule.armed && 'opacity-50',
        rule.flash && 'chaos-rule-flash',
      )}
    >
      <div className="flex items-center gap-2">
        <MiniToggle on={rule.armed} label={s.armToggle} onToggle={onToggle} />
        <span
          className={
            'text-section min-w-0 truncate ' +
            (rule.name
              ? 'text-foreground'
              : 'text-muted-foreground font-mono italic')
          }
        >
          {ruleDisplayName(rule)}
        </span>
        <span className="ml-auto flex shrink-0 gap-1">
          <Button
            variant="ghost"
            size="icon"
            title={s.edit}
            aria-label={s.edit}
            className="h-6 w-6"
            onClick={onEdit}
          >
            <Pencil className="h-3 w-3" />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            title={s.delete}
            aria-label={s.delete}
            className="text-muted-foreground hover:text-destructive hover:bg-destructive/10 h-6 w-6"
            onClick={onDelete}
          >
            <Trash2 className="h-3 w-3" />
          </Button>
        </span>
      </div>

      <div className="text-meta text-muted-foreground mb-2 mt-1 truncate font-mono">
        {meta}
      </div>

      <div className="flex h-[22px] items-end gap-0.5">
        {rule.spark.map((bucket) => {
          const isLatest = bucket === rule.spark[rule.spark.length - 1]
          return (
            <span
              key={bucket.id}
              className={cn(
                'w-[7px] shrink-0 rounded-t-sm',
                isLatest ? 'bg-chaos chaos-pulse' : 'bg-primary',
              )}
              style={{
                height: `${Math.max(MIN_BAR_HEIGHT_PCT, Math.round((bucket.value / sparkMax) * 100))}%`,
              }}
            />
          )
        })}
        <span
          className={cn(
            'ml-auto font-mono font-bold leading-none',
            rule.armed ? 'text-chaos' : 'text-muted-foreground',
          )}
          style={{ fontSize: 15 }}
        >
          {rule.hits.toLocaleString()}
        </span>
        <span className="text-muted-foreground pb-px" style={{ fontSize: 10 }}>
          {s.hitsLabel}
        </span>
      </div>
    </div>
  )
}
