import { useState } from 'react'
import { Button, cn } from '@metalbear/ui'
import type { ChaosEffectKind } from '../../types'
import type { ChaosRuleFields } from '../../hooks/useChaosRules'
import { strings } from '../../strings'

const EFFECT_OPTIONS: { kind: ChaosEffectKind; label: string }[] = [
  { kind: 'latency', label: strings.chaos.effectLatency },
  { kind: 'reset', label: strings.chaos.effectReset },
  { kind: 'timed_out', label: strings.chaos.effectTimeout },
  { kind: 'refused', label: strings.chaos.effectRefuse },
]

interface DraftState {
  name: string
  upstream: string
  effectKind: ChaosEffectKind
  readMs: string
  writeMs: string
  jitterMs: string
  afterMs: string
  percentage: number
  priority: string
}

function toDraft(fields: ChaosRuleFields): DraftState {
  return {
    name: fields.name,
    upstream: fields.upstream,
    effectKind: fields.effectKind,
    readMs: fields.readMs ? String(fields.readMs) : '',
    writeMs: fields.writeMs ? String(fields.writeMs) : '',
    jitterMs: fields.jitterMs ? String(fields.jitterMs) : '',
    afterMs: fields.afterMs ? String(fields.afterMs) : '',
    percentage: fields.percentage,
    priority: fields.priority ? String(fields.priority) : '',
  }
}

function toInt(value: string): number {
  const n = parseInt(value, 10)
  return Number.isFinite(n) && n > 0 ? n : 0
}

const FIELD_BOX =
  'bg-card border-border flex items-center gap-1.5 rounded-md border px-2 py-1'
const FIELD_INPUT =
  'text-body text-foreground min-w-0 bg-transparent font-mono outline-none'
const FIELD_UNIT = 'text-meta text-muted-foreground shrink-0'

interface ChaosRuleFormProps {
  initial: ChaosRuleFields
  isEdit: boolean
  seenHosts: string[]
  onCancel: () => void
  onSubmit: (fields: ChaosRuleFields) => Promise<void>
}

export default function ChaosRuleForm({
  initial,
  isEdit,
  seenHosts,
  onCancel,
  onSubmit,
}: ChaosRuleFormProps) {
  const s = strings.chaos
  const [draft, setDraft] = useState<DraftState>(() => toDraft(initial))
  const [error, setError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  function set<K extends keyof DraftState>(key: K, value: DraftState[K]) {
    setDraft((prev) => ({ ...prev, [key]: value }))
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault()
    if (!draft.upstream.trim()) {
      setError(s.upstreamRequired)
      return
    }
    const fields: ChaosRuleFields = {
      name: draft.name.trim(),
      upstream: draft.upstream.trim(),
      effectKind: draft.effectKind,
      readMs: toInt(draft.readMs),
      writeMs: toInt(draft.writeMs),
      jitterMs: toInt(draft.jitterMs),
      afterMs: toInt(draft.afterMs),
      percentage: Math.min(100, Math.max(0, draft.percentage)),
      priority: toInt(draft.priority),
    }
    if (fields.effectKind === 'latency' && !fields.readMs && !fields.writeMs) {
      setError(s.latencyValidation)
      return
    }
    setSubmitting(true)
    setError(null)
    try {
      await onSubmit(fields)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
      setSubmitting(false)
    }
  }

  return (
    <form
      onSubmit={(e) => void handleSubmit(e)}
      className="border-primary rounded-lg border bg-muted/30 p-3"
    >
      <div className="text-section text-foreground mb-2.5">
        {isEdit ? s.formTitleEdit : s.formTitleCreate}
      </div>

      <div className="text-meta text-muted-foreground mb-1">
        {s.fieldRuleType}
      </div>
      <div className="mb-3 grid grid-cols-2 gap-1.5">
        <div className="border-primary bg-primary/10 flex items-start gap-2 rounded-md border p-2.5">
          <span className="border-primary mt-0.5 flex h-3 w-3 shrink-0 items-center justify-center rounded-full border">
            <span className="bg-primary h-1.5 w-1.5 rounded-full" />
          </span>
          <span className="min-w-0">
            <span className="text-body text-foreground block font-semibold">
              {s.typeTcpTitle}
            </span>
            <span className="text-meta text-muted-foreground block">
              {s.typeTcpDesc}
            </span>
          </span>
        </div>
        <div className="border-border flex items-start gap-2 rounded-md border p-2.5 opacity-60">
          <span className="border-border mt-0.5 h-3 w-3 shrink-0 rounded-full border" />
          <span className="min-w-0">
            <span className="text-body text-muted-foreground flex items-center gap-1.5 font-semibold">
              {s.typeFsTitle}
              <span
                className="border-border text-muted-foreground shrink-0 rounded-full border px-1.5 font-medium"
                style={{ fontSize: 10 }}
              >
                {s.comingSoon}
              </span>
            </span>
            <span className="text-meta text-muted-foreground block">
              {s.typeFsDesc}
            </span>
          </span>
        </div>
      </div>

      <label
        htmlFor="chaos-upstream"
        className="text-meta text-muted-foreground mb-1 block"
      >
        {s.fieldUpstream}
      </label>
      <input
        id="chaos-upstream"
        value={draft.upstream}
        placeholder={s.upstreamPlaceholder}
        onChange={(e) => set('upstream', e.target.value)}
        className="bg-card border-border text-body text-foreground focus-visible:ring-ring mb-1.5 w-full rounded-md border px-2.5 py-1.5 font-mono focus-visible:outline-none focus-visible:ring-1"
      />
      {seenHosts.length > 0 && (
        <div className="mb-3 flex flex-wrap items-center gap-1.5">
          <span className="text-meta text-muted-foreground w-full">
            {s.seenThisSession}
          </span>
          {seenHosts.map((host) => (
            <button
              key={host}
              type="button"
              onClick={() => set('upstream', host)}
              className={
                'text-meta rounded-full border px-2 py-0.5 font-mono transition-colors ' +
                (draft.upstream === host
                  ? 'border-primary bg-primary/20 text-foreground'
                  : 'border-border text-muted-foreground hover:text-foreground')
              }
            >
              {host}
            </button>
          ))}
        </div>
      )}

      <div className="text-meta text-muted-foreground mb-1">
        {s.fieldEffect}
      </div>
      <div className="mb-2 flex gap-1.5">
        {EFFECT_OPTIONS.map((option) => (
          <button
            key={option.kind}
            type="button"
            onClick={() => set('effectKind', option.kind)}
            className={
              'text-meta flex-1 rounded-md border py-1 text-center font-medium transition-colors ' +
              (draft.effectKind === option.kind
                ? 'border-primary bg-primary/25 text-foreground'
                : 'border-border text-muted-foreground hover:text-foreground')
            }
          >
            {option.label}
          </button>
        ))}
      </div>

      {draft.effectKind === 'latency' ? (
        <div className="mb-3 flex gap-1.5">
          <label className={cn(FIELD_BOX, 'flex-1')}>
            <input
              value={draft.readMs}
              onChange={(e) => set('readMs', e.target.value)}
              className={FIELD_INPUT + ' w-9'}
              inputMode="numeric"
            />
            <span className={FIELD_UNIT}>{s.unitReadMs}</span>
          </label>
          <label className={cn(FIELD_BOX, 'flex-1')}>
            <input
              value={draft.writeMs}
              onChange={(e) => set('writeMs', e.target.value)}
              className={FIELD_INPUT + ' w-9'}
              inputMode="numeric"
            />
            <span className={FIELD_UNIT}>{s.unitWriteMs}</span>
          </label>
          <label className={cn(FIELD_BOX, 'flex-1')}>
            <input
              value={draft.jitterMs}
              onChange={(e) => set('jitterMs', e.target.value)}
              className={FIELD_INPUT + ' w-8'}
              inputMode="numeric"
            />
            <span className={FIELD_UNIT}>{s.unitJitter}</span>
          </label>
        </div>
      ) : (
        <div className="mb-3">
          <label className={cn(FIELD_BOX, 'inline-flex')}>
            <span className={FIELD_UNIT}>{s.unitAfter}</span>
            <input
              value={draft.afterMs}
              onChange={(e) => set('afterMs', e.target.value)}
              className={FIELD_INPUT + ' w-11'}
              inputMode="numeric"
            />
            <span className={FIELD_UNIT}>{s.unitMsOptional}</span>
          </label>
        </div>
      )}

      <div className="text-meta text-muted-foreground mb-1">
        {s.fieldTraffic}
      </div>
      <div className="mb-1 flex items-center gap-2.5">
        <input
          type="range"
          min={0}
          max={100}
          value={draft.percentage}
          onChange={(e) => set('percentage', Number(e.target.value))}
          className="accent-primary min-w-0 flex-1"
          aria-label={s.fieldTraffic}
        />
        <span className="text-body w-10 shrink-0 text-right font-mono">
          {s.pctOf(draft.percentage)}
        </span>
      </div>
      <div className="text-meta text-muted-foreground/70 mb-3">
        {s.trafficHint(draft.percentage)}
      </div>

      <div className="mb-3 flex gap-1.5">
        <input
          value={draft.name}
          placeholder={s.namePlaceholder}
          onChange={(e) => set('name', e.target.value)}
          className="bg-card border-border text-body text-foreground focus-visible:ring-ring min-w-0 flex-[1.4] rounded-md border px-2 py-1 focus-visible:outline-none focus-visible:ring-1"
        />
        <label className={cn(FIELD_BOX, 'flex-[0.6]')}>
          <input
            value={draft.priority}
            onChange={(e) => set('priority', e.target.value)}
            className={FIELD_INPUT + ' w-7 flex-1'}
            inputMode="numeric"
          />
          <span className={FIELD_UNIT}>{s.unitPrio}</span>
        </label>
      </div>

      {error && <p className="text-meta text-destructive mb-2">{error}</p>}

      <div className="flex items-center justify-end gap-3">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="text-muted-foreground h-7"
          onClick={onCancel}
        >
          {s.cancel}
        </Button>
        <Button
          type="submit"
          size="sm"
          variant="outline"
          className="h-7"
          disabled={submitting}
        >
          {isEdit ? s.formCtaEdit : s.formCtaCreate}
        </Button>
      </div>
    </form>
  )
}
