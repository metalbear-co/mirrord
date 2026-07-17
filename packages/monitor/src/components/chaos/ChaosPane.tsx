import { useEffect, useState } from 'react'
import { FlaskConical } from 'lucide-react'
import { Button } from '@metalbear/ui'
import type { ClientChaosRule } from '../../types'
import type { ChaosRuleFields, UseChaosRules } from '../../hooks/useChaosRules'
import { strings } from '../../strings'
import ChaosRuleCard from './ChaosRuleCard'
import ChaosRuleForm from './ChaosRuleForm'
import MiniToggle from './MiniToggle'
import RequestChaosTypeDialog from './RequestChaosTypeDialog'

export interface ChaosFormRequest {
  upstream: string
  nonce: number
}

function defaultFields(upstream: string): ChaosRuleFields {
  return {
    name: '',
    upstream,
    effectKind: 'latency',
    readMs: 800,
    writeMs: 200,
    jitterMs: 150,
    afterMs: 0,
    percentage: 50,
    priority: 0,
  }
}

function fieldsFromRule(rule: ClientChaosRule): ChaosRuleFields {
  return {
    name: rule.name,
    upstream: rule.upstream,
    effectKind: rule.effectKind,
    readMs: rule.readMs,
    writeMs: rule.writeMs,
    jitterMs: rule.jitterMs,
    afterMs: rule.afterMs,
    percentage: rule.percentage,
    priority: rule.priority,
  }
}

interface FormState {
  editKey: string | null
  initial: ChaosRuleFields
}

interface ChaosPaneProps {
  chaos: UseChaosRules
  seenHosts: string[]
  formRequest: ChaosFormRequest | null
}

export default function ChaosPane({
  chaos,
  seenHosts,
  formRequest,
}: ChaosPaneProps) {
  const s = strings.chaos
  const [form, setForm] = useState<FormState | null>(null)

  useEffect(() => {
    if (!formRequest) return
    setForm({ editKey: null, initial: defaultFields(formRequest.upstream) })
  }, [formRequest])

  async function handleSubmit(fields: ChaosRuleFields) {
    if (form?.editKey) await chaos.updateRule(form.editKey, fields)
    else await chaos.createRule(fields)
    setForm(null)
  }

  const showEmpty = chaos.rules.length === 0 && !form

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-y-auto">
      {chaos.armedCount > 0 && (
        <div className="border-border bg-chaos/5 flex shrink-0 items-center gap-2.5 border-b px-3.5 py-2">
          <span className="text-meta text-chaos font-semibold">
            {s.armedSummary(chaos.armedCount, chaos.totalHits.toLocaleString())}
          </span>
          <span className="text-meta text-muted-foreground ml-auto">
            {s.disarmAll}
          </span>
          <MiniToggle
            on
            label={s.disarmAll}
            onToggle={() => void chaos.disarmAll()}
          />
        </div>
      )}

      {chaos.loadError && (
        <p className="text-meta text-destructive shrink-0 px-3.5 pt-2">
          {s.loadFailed}
        </p>
      )}

      <div className="flex flex-col gap-2.5 p-3">
        {form && (
          <ChaosRuleForm
            key={form.editKey ?? `new-${formRequest?.nonce ?? 0}`}
            initial={form.initial}
            isEdit={form.editKey !== null}
            seenHosts={seenHosts}
            onCancel={() => setForm(null)}
            onSubmit={handleSubmit}
          />
        )}

        {chaos.rules.map((rule) => (
          <ChaosRuleCard
            key={rule.key}
            rule={rule}
            onToggle={() => void chaos.toggleRule(rule.key)}
            onEdit={() =>
              setForm({ editKey: rule.key, initial: fieldsFromRule(rule) })
            }
            onDelete={() => void chaos.deleteRule(rule.key)}
          />
        ))}

        {showEmpty && (
          <div className="px-3 py-5 text-center">
            <FlaskConical className="text-primary mx-auto mb-2 h-6 w-6" />
            <div className="text-title text-foreground mb-1.5">
              {s.emptyTitle}
            </div>
            <div className="text-meta text-muted-foreground mb-3.5 leading-relaxed">
              {s.emptyBodyLead}
              <b className="text-foreground">{s.emptyBodyStrong}</b>
              {s.emptyBodyRest}
            </div>
            <Button
              size="sm"
              variant="outline"
              className="mb-3.5 h-7"
              onClick={() =>
                setForm({ editKey: null, initial: defaultFields('') })
              }
            >
              {s.newRule}
            </Button>
            <div className="border-border text-meta text-muted-foreground rounded-lg border border-dashed p-2.5">
              {s.emptyHintLead}
              <span className="font-mono">{s.emptyHintOut}</span>
              {s.emptyHintMid}
              <b className="text-foreground">{s.emptyHintStrong}</b>
            </div>
          </div>
        )}
      </div>

      <div className="border-border text-meta text-muted-foreground/70 mt-auto shrink-0 border-t px-3.5 py-2 text-center">
        {s.footerNote} <RequestChaosTypeDialog />
      </div>
    </div>
  )
}
