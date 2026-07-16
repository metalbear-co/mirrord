import { useCallback, useEffect, useState } from 'react'
import {
  Button,
  Table,
  TableBody,
  TableHead,
  TableHeader,
  TableRow,
} from '@metalbear/ui'
import { FlaskConical, Lightbulb, Plus } from 'lucide-react'
import { api } from '../../api'
import { strings } from '../../strings'
import type { ChaosRule, ChaosRuleRequest } from '../../types'
import { emitUserBlocked } from '../../analytics'
import Widget from '../Widget'
import ChaosRuleForm from './ChaosRuleForm'
import ChaosRuleRow from './ChaosRuleRow'
import RequestChaosTypeDialog from './RequestChaosTypeDialog'

// Refetching the rule list is how we observe live hit counts — `hit_count` is just a normal field
// on `ChaosRule`, there's no separate stats endpoint.
const POLL_INTERVAL_MS = 2500

interface ChaosRulesTabProps {
  sessionId: string
}

export default function ChaosRulesTab({ sessionId }: ChaosRulesTabProps) {
  const s = strings.chaos
  const [rules, setRules] = useState<ChaosRule[] | null>(null)
  const [loadError, setLoadError] = useState(false)
  const [formOpen, setFormOpen] = useState(false)
  const [editingRule, setEditingRule] = useState<ChaosRule | null>(null)

  const refresh = useCallback(async () => {
    try {
      const list = await api.listChaosRules(sessionId)
      setRules(list)
      setLoadError(false)
    } catch (err) {
      setLoadError(true)
      emitUserBlocked('chaos_rules_load_failed', 'user_action', {
        session_id: sessionId,
        error: err instanceof Error ? err.message : String(err),
      })
    }
  }, [sessionId])

  useEffect(() => {
    setRules(null)
    setLoadError(false)
    void refresh()
    const interval = setInterval(() => void refresh(), POLL_INTERVAL_MS)
    return () => clearInterval(interval)
  }, [refresh])

  function openCreate() {
    setEditingRule(null)
    setFormOpen(true)
  }

  function openEdit(rule: ChaosRule) {
    setEditingRule(rule)
    setFormOpen(true)
  }

  async function handleSubmit(request: ChaosRuleRequest) {
    if (editingRule) {
      await api.updateChaosRule(sessionId, editingRule.id, request)
    } else {
      await api.createChaosRule(sessionId, request)
    }
    await refresh()
  }

  async function handleDelete(rule: ChaosRule) {
    await api.deleteChaosRule(sessionId, rule.id)
    await refresh()
  }

  return (
    <div className="absolute inset-0 flex flex-col p-4 gap-4">
      {loadError && <p className="text-meta text-destructive shrink-0">{s.loadFailed}</p>}

      {rules?.length === 0 ? (
        <div className="flex-1 min-h-0 flex items-center justify-center text-center p-6">
          <div className="max-w-sm flex flex-col items-center gap-3">
            <FlaskConical className="h-6 w-6 text-muted-foreground/40" />
            <h3 className="text-base font-semibold text-foreground">{s.emptyTitle}</h3>
            <p className="text-xs text-muted-foreground">{s.emptyBody}</p>
            <Button size="sm" onClick={openCreate}>
              <Plus className="h-3.5 w-3.5 mr-1.5" />
              {s.newRule}
            </Button>
          </div>
        </div>
      ) : (
        rules &&
        rules.length > 0 && (
          <Widget
            className="flex-1 min-h-0"
            trailing={
              <Button size="sm" onClick={openCreate}>
                <Plus className="h-3.5 w-3.5 mr-1.5" />
                {s.newRule}
              </Button>
            }
          >
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{s.columnName}</TableHead>
                  <TableHead>{s.columnUpstream}</TableHead>
                  <TableHead>{s.columnEffect}</TableHead>
                  <TableHead>{s.columnPercentage}</TableHead>
                  <TableHead>{s.columnPriority}</TableHead>
                  <TableHead>{s.columnHits}</TableHead>
                  <TableHead />
                </TableRow>
              </TableHeader>
              <TableBody>
                {rules.map((rule) => (
                  <ChaosRuleRow
                    key={rule.id}
                    rule={rule}
                    onEdit={() => openEdit(rule)}
                    onDelete={() => void handleDelete(rule)}
                  />
                ))}
              </TableBody>
            </Table>
          </Widget>
        )
      )}

      <div className="flex shrink-0 items-center justify-between gap-4 rounded-lg border border-primary/20 bg-primary/5 px-4 py-3">
        <div className="flex items-center gap-3">
          <Lightbulb className="h-5 w-5 shrink-0 text-primary" />
          <div>
            <p className="text-sm font-semibold text-foreground">{s.selectorNoteTitle}</p>
            <p className="text-meta text-muted-foreground">{s.selectorNote}</p>
          </div>
        </div>
        <RequestChaosTypeDialog />
      </div>

      <ChaosRuleForm
        open={formOpen}
        onOpenChange={setFormOpen}
        initialRule={editingRule}
        onSubmit={handleSubmit}
      />
    </div>
  )
}
