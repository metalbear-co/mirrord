import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from '../api'
import { emitUserBlocked } from '../analytics'
import type {
  ChaosEffectKind,
  ChaosRule,
  ChaosRuleRequest,
  ClientChaosRule,
} from '../types'

// Hit counts arrive by refetching the rule list — `hit_count` is a plain field on
// `ChaosRule`, there is no stats endpoint. Each poll's delta feeds one sparkline bucket.
const POLL_INTERVAL_MS = 2500
const SPARK_BUCKETS = 9
const FLASH_MS = 1600

let nextBucketId = 0

export interface ChaosRuleFields {
  name: string
  upstream: string
  effectKind: ChaosEffectKind
  readMs: number
  writeMs: number
  jitterMs: number
  afterMs: number
  percentage: number
  priority: number
}

export interface UseChaosRules {
  rules: ClientChaosRule[]
  loadError: boolean
  armedCount: number
  totalHits: number
  createRule: (fields: ChaosRuleFields) => Promise<void>
  updateRule: (key: string, fields: ChaosRuleFields) => Promise<void>
  deleteRule: (key: string) => Promise<void>
  toggleRule: (key: string) => Promise<void>
  disarmAll: () => Promise<void>
}

function toRequest(fields: ChaosRuleFields): ChaosRuleRequest {
  const effect =
    fields.effectKind === 'latency'
      ? {
          latency: {
            read_ms: fields.readMs || undefined,
            write_ms: fields.writeMs || undefined,
            jitter_ms: fields.jitterMs || undefined,
          },
        }
      : {
          connection_error: {
            type: fields.effectKind,
            after_ms: fields.afterMs || undefined,
          },
        }
  return {
    name: fields.name.trim() || undefined,
    priority: fields.priority,
    effect,
    selector: {
      upstream: fields.upstream.trim(),
      percentage: fields.percentage,
    },
  }
}

function fromServer(rule: ChaosRule): ClientChaosRule | null {
  if (rule.selector.type !== 'tcp') return null
  const effect = rule.selector.effect
  const base: ClientChaosRule = {
    key: `srv-${rule.id}`,
    serverId: rule.id,
    name: rule.name ?? '',
    upstream: rule.selector.upstream,
    effectKind: 'latency',
    readMs: 0,
    writeMs: 0,
    jitterMs: 0,
    afterMs: 0,
    percentage: rule.selector.percentage,
    priority: rule.priority,
    armed: true,
    hits: rule.hit_count,
    serverHits: rule.hit_count,
    spark: [],
    flash: false,
  }
  if ('latency' in effect) {
    return {
      ...base,
      readMs: effect.latency.read_ms ?? 0,
      writeMs: effect.latency.write_ms ?? 0,
      jitterMs: effect.latency.jitter_ms ?? 0,
    }
  }
  return {
    ...base,
    effectKind: effect.connection_error.error_type,
    afterMs: effect.connection_error.after_ms ?? 0,
  }
}

export function useChaosRules(sessionId: string): UseChaosRules {
  const [rules, setRules] = useState<ClientChaosRule[]>([])
  const [loadError, setLoadError] = useState(false)
  // Poll results fetched before the latest mutation are stale — bumping this
  // sequence on every mutation lets an in-flight merge detect and drop itself.
  const mutationSeq = useRef(0)
  const loadErrorRef = useRef(false)
  const rulesRef = useRef(rules)
  rulesRef.current = rules

  const flash = useCallback((key: string) => {
    setTimeout(() => {
      setRules((prev) =>
        prev.map((r) => (r.key === key ? { ...r, flash: false } : r)),
      )
    }, FLASH_MS)
  }, [])

  const poll = useCallback(async () => {
    const seq = mutationSeq.current
    let serverRules: ChaosRule[]
    try {
      serverRules = await api.listChaosRules(sessionId)
      setLoadError(false)
      loadErrorRef.current = false
    } catch (err) {
      setLoadError(true)
      if (!loadErrorRef.current) {
        loadErrorRef.current = true
        emitUserBlocked(
          'chaos_rules_load_failed',
          'user_action',
          {
            session_id: sessionId,
            error: err instanceof Error ? err.message : String(err),
          },
          err,
        )
      }
      return
    }
    if (seq !== mutationSeq.current) return

    setRules((prev) => {
      const byServerId = new Map(serverRules.map((r) => [r.id, r]))
      const known = new Set(
        prev.map((r) => r.serverId).filter((id): id is string => id !== null),
      )
      const next: ClientChaosRule[] = []
      for (const rule of prev) {
        if (!rule.armed || rule.serverId === null) {
          next.push(rule)
          continue
        }
        const server = byServerId.get(rule.serverId)
        if (!server) continue
        const delta = Math.max(0, server.hit_count - rule.serverHits)
        next.push({
          ...rule,
          hits: rule.hits + delta,
          serverHits: server.hit_count,
          spark: [...rule.spark, { id: nextBucketId++, value: delta }].slice(
            -SPARK_BUCKETS,
          ),
        })
      }
      for (const server of serverRules) {
        if (known.has(server.id)) continue
        const mapped = fromServer(server)
        if (mapped) next.push(mapped)
      }
      return next
    })
  }, [sessionId])

  useEffect(() => {
    setRules([])
    setLoadError(false)
    loadErrorRef.current = false
    void poll()
    const interval = setInterval(() => void poll(), POLL_INTERVAL_MS)
    return () => clearInterval(interval)
  }, [poll])

  const createRule = useCallback(
    async (fields: ChaosRuleFields) => {
      const created = await api.createChaosRule(sessionId, toRequest(fields))
      mutationSeq.current++
      const key = `srv-${created.id}`
      setRules((prev) => [
        {
          ...fields,
          key,
          serverId: created.id,
          name: fields.name.trim(),
          upstream: fields.upstream.trim(),
          armed: true,
          hits: 0,
          serverHits: 0,
          spark: [],
          flash: true,
        },
        ...prev,
      ])
      flash(key)
    },
    [sessionId, flash],
  )

  const updateRule = useCallback(
    async (key: string, fields: ChaosRuleFields) => {
      const rule = rulesRef.current.find((r) => r.key === key)
      if (!rule) return
      if (rule.armed && rule.serverId !== null) {
        await api.updateChaosRule(sessionId, rule.serverId, toRequest(fields))
        mutationSeq.current++
      }
      setRules((prev) =>
        prev.map((r) =>
          r.key === key
            ? {
                ...r,
                ...fields,
                name: fields.name.trim(),
                upstream: fields.upstream.trim(),
              }
            : r,
        ),
      )
    },
    [sessionId],
  )

  const deleteRule = useCallback(
    async (key: string) => {
      const rule = rulesRef.current.find((r) => r.key === key)
      if (!rule) return
      if (rule.armed && rule.serverId !== null) {
        await api.deleteChaosRule(sessionId, rule.serverId)
        mutationSeq.current++
      }
      setRules((prev) => prev.filter((r) => r.key !== key))
    },
    [sessionId],
  )

  const pause = useCallback(
    async (rule: ClientChaosRule) => {
      if (rule.serverId !== null) {
        await api.deleteChaosRule(sessionId, rule.serverId)
        mutationSeq.current++
      }
      setRules((prev) =>
        prev.map((r) =>
          r.key === rule.key ? { ...r, armed: false, serverId: null } : r,
        ),
      )
    },
    [sessionId],
  )

  const arm = useCallback(
    async (rule: ClientChaosRule) => {
      const created = await api.createChaosRule(sessionId, toRequest(rule))
      mutationSeq.current++
      setRules((prev) =>
        prev.map((r) =>
          r.key === rule.key
            ? { ...r, armed: true, serverId: created.id, serverHits: 0 }
            : r,
        ),
      )
    },
    [sessionId],
  )

  const toggleRule = useCallback(
    async (key: string) => {
      const rule = rulesRef.current.find((r) => r.key === key)
      if (!rule) return
      if (rule.armed) await pause(rule)
      else await arm(rule)
    },
    [pause, arm],
  )

  const disarmAll = useCallback(async () => {
    const armed = rulesRef.current.filter((r) => r.armed)
    await Promise.all(armed.map((r) => pause(r)))
  }, [pause])

  const armedCount = rules.filter((r) => r.armed).length
  const totalHits = rules.reduce((sum, r) => sum + r.hits, 0)

  return {
    rules,
    loadError,
    armedCount,
    totalHits,
    createRule,
    updateRule,
    deleteRule,
    toggleRule,
    disarmAll,
  }
}
