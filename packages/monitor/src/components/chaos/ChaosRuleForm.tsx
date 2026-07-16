import { useEffect, useState } from 'react'
import {
  Button,
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
  Label,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@metalbear/ui'
import type { ChaosRule, ChaosRuleRequest, ConnectionErrorType } from '../../types'
import { strings } from '../../strings'

type EffectKind = 'latency' | 'connection_error'

interface FormState {
  name: string
  upstream: string
  percentage: string
  priority: string
  effectKind: EffectKind
  readMs: string
  writeMs: string
  jitterMs: string
  errorType: ConnectionErrorType
  afterMs: string
}

function emptyState(): FormState {
  return {
    name: '',
    upstream: '',
    percentage: '100',
    priority: '0',
    effectKind: 'latency',
    readMs: '',
    writeMs: '',
    jitterMs: '',
    errorType: 'reset',
    afterMs: '',
  }
}

function stateFromRule(rule: ChaosRule): FormState {
  const base = emptyState()
  base.name = rule.name ?? ''
  base.priority = String(rule.priority)
  if (rule.selector.type !== 'tcp') return base

  base.upstream = rule.selector.upstream
  base.percentage = String(rule.selector.percentage)

  const effect = rule.selector.effect
  if ('latency' in effect) {
    base.effectKind = 'latency'
    base.readMs = effect.latency.read_ms ? String(effect.latency.read_ms) : ''
    base.writeMs = effect.latency.write_ms ? String(effect.latency.write_ms) : ''
    base.jitterMs = effect.latency.jitter_ms ? String(effect.latency.jitter_ms) : ''
  } else {
    base.effectKind = 'connection_error'
    base.errorType = effect.connection_error.error_type
    base.afterMs = effect.connection_error.after_ms ? String(effect.connection_error.after_ms) : ''
  }
  return base
}

function toNumberOrUndefined(value: string): number | undefined {
  const trimmed = value.trim()
  if (!trimmed) return undefined
  const n = Number(trimmed)
  return Number.isFinite(n) ? n : undefined
}

interface ChaosRuleFormProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** Rule being edited, or null/undefined to create a new one. */
  initialRule?: ChaosRule | null
  onSubmit: (request: ChaosRuleRequest) => Promise<void>
}

export default function ChaosRuleForm({
  open,
  onOpenChange,
  initialRule,
  onSubmit,
}: ChaosRuleFormProps) {
  const s = strings.chaos
  const [form, setForm] = useState<FormState>(() => (initialRule ? stateFromRule(initialRule) : emptyState()))
  const [error, setError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  // Reset to the right initial values every time the dialog opens (create vs. edit).
  useEffect(() => {
    if (!open) return
    setForm(initialRule ? stateFromRule(initialRule) : emptyState())
    setError(null)
  }, [open, initialRule])

  function set<K extends keyof FormState>(key: K, value: FormState[K]) {
    setForm((prev) => ({ ...prev, [key]: value }))
  }

  function buildRequest(): ChaosRuleRequest | string {
    if (!form.upstream.trim()) return s.upstreamRequired

    const effect =
      form.effectKind === 'latency'
        ? (() => {
            const read_ms = toNumberOrUndefined(form.readMs)
            const write_ms = toNumberOrUndefined(form.writeMs)
            if (!read_ms && !write_ms) return s.latencyValidation
            return {
              latency: { read_ms, write_ms, jitter_ms: toNumberOrUndefined(form.jitterMs) },
            }
          })()
        : {
            connection_error: {
              type: form.errorType,
              after_ms: toNumberOrUndefined(form.afterMs),
            },
          }

    if (typeof effect === 'string') return effect

    return {
      name: form.name.trim() || undefined,
      priority: toNumberOrUndefined(form.priority),
      effect,
      selector: {
        upstream: form.upstream.trim(),
        percentage: toNumberOrUndefined(form.percentage),
      },
    }
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault()
    const request = buildRequest()
    if (typeof request === 'string') {
      setError(request)
      return
    }

    setSubmitting(true)
    setError(null)
    try {
      await onSubmit(request)
      onOpenChange(false)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{initialRule ? s.formTitleEdit : s.formTitleCreate}</DialogTitle>
        </DialogHeader>

        <form onSubmit={(e) => void handleSubmit(e)} className="flex flex-col gap-4">
          <div className="grid grid-cols-2 gap-4">
            <div className="col-span-2 flex flex-col gap-1.5">
              <Label htmlFor="chaos-name">{s.fieldName}</Label>
              <Input
                id="chaos-name"
                value={form.name}
                placeholder={s.fieldNamePlaceholder}
                onChange={(e) => set('name', e.target.value)}
              />
            </div>

            <div className="col-span-2 flex flex-col gap-1.5">
              <Label>{s.fieldSelectorType}</Label>
              {/* Only the TCP selector is implemented server-side today. The "File" option is
                  shown disabled so the form previews the roadmap without letting anyone submit
                  a selector the backend would reject. */}
              <Select value="tcp">
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="tcp">{s.selectorTypeTcp}</SelectItem>
                  <SelectItem value="fs" disabled>
                    {s.selectorTypeFs}
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>

            <div className="col-span-2 flex flex-col gap-1.5">
              <Label htmlFor="chaos-upstream">{s.fieldUpstream}</Label>
              <Input
                id="chaos-upstream"
                required
                className="font-mono"
                value={form.upstream}
                placeholder={s.fieldUpstreamPlaceholder}
                onChange={(e) => set('upstream', e.target.value)}
              />
              <p className="text-meta text-muted-foreground">{s.fieldUpstreamHint}</p>
            </div>

            <div className="flex flex-col gap-1.5">
              <Label htmlFor="chaos-percentage">{s.fieldPercentage}</Label>
              <Input
                id="chaos-percentage"
                type="number"
                min={0}
                max={100}
                value={form.percentage}
                onChange={(e) => set('percentage', e.target.value)}
              />
            </div>

            <div className="flex flex-col gap-1.5">
              <Label htmlFor="chaos-priority">{s.fieldPriority}</Label>
              <Input
                id="chaos-priority"
                type="number"
                min={0}
                value={form.priority}
                onChange={(e) => set('priority', e.target.value)}
              />
            </div>

            <div className="col-span-2 flex flex-col gap-1.5">
              <Label>{s.fieldEffect}</Label>
              <Select
                value={form.effectKind}
                onValueChange={(value: EffectKind) => set('effectKind', value)}
              >
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="latency">{s.effectLatency}</SelectItem>
                  <SelectItem value="connection_error">{s.effectConnectionError}</SelectItem>
                </SelectContent>
              </Select>
            </div>

            {form.effectKind === 'latency' ? (
              <>
                <div className="flex flex-col gap-1.5">
                  <Label htmlFor="chaos-read-ms">{s.fieldReadMs}</Label>
                  <Input
                    id="chaos-read-ms"
                    type="number"
                    min={0}
                    value={form.readMs}
                    onChange={(e) => set('readMs', e.target.value)}
                  />
                </div>
                <div className="flex flex-col gap-1.5">
                  <Label htmlFor="chaos-write-ms">{s.fieldWriteMs}</Label>
                  <Input
                    id="chaos-write-ms"
                    type="number"
                    min={0}
                    value={form.writeMs}
                    onChange={(e) => set('writeMs', e.target.value)}
                  />
                </div>
                <div className="flex flex-col gap-1.5">
                  <Label htmlFor="chaos-jitter-ms">{s.fieldJitterMs}</Label>
                  <Input
                    id="chaos-jitter-ms"
                    type="number"
                    min={0}
                    value={form.jitterMs}
                    onChange={(e) => set('jitterMs', e.target.value)}
                  />
                </div>
              </>
            ) : (
              <>
                <div className="flex flex-col gap-1.5">
                  <Label>{s.fieldErrorType}</Label>
                  <Select
                    value={form.errorType}
                    onValueChange={(value: ConnectionErrorType) => set('errorType', value)}
                  >
                    <SelectTrigger>
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="reset">{s.errorTypeReset}</SelectItem>
                      <SelectItem value="timed_out">{s.errorTypeTimedOut}</SelectItem>
                      <SelectItem value="refused">{s.errorTypeRefused}</SelectItem>
                    </SelectContent>
                  </Select>
                </div>
                <div className="flex flex-col gap-1.5">
                  <Label htmlFor="chaos-after-ms">{s.fieldAfterMs}</Label>
                  <Input
                    id="chaos-after-ms"
                    type="number"
                    min={0}
                    value={form.afterMs}
                    onChange={(e) => set('afterMs', e.target.value)}
                  />
                </div>
              </>
            )}
          </div>

          {error && <p className="text-meta text-destructive">{error}</p>}

          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              {s.cancel}
            </Button>
            <Button type="submit" disabled={submitting}>
              {submitting ? s.submitting : initialRule ? s.submitEdit : s.submitCreate}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
