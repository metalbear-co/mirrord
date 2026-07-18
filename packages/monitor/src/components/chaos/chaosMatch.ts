import type { ClientChaosRule } from '../../types'
import { stripPortSuffix } from '../../utils'

export function splitUpstream(upstream: string): {
  host: string
  port: number | null
} {
  const idx = upstream.lastIndexOf(':')
  if (idx === -1) return { host: upstream, port: null }
  const port = Number(upstream.slice(idx + 1))
  if (!Number.isInteger(port)) return { host: upstream, port: null }
  return { host: upstream.slice(0, idx), port }
}

// The backend doesn't say which individual connections a rule affected (only the
// aggregate hit counter), so the stream tags every outgoing event a rule *targets*:
// the highest-priority armed rule whose host (and port, when given) matches.
export function matchChaosRule(
  rules: ClientChaosRule[],
  address: string,
  port: number,
): ClientChaosRule | null {
  const host = stripPortSuffix(address, port)
  let best: ClientChaosRule | null = null
  for (const rule of rules) {
    if (!rule.armed) continue
    const target = splitUpstream(rule.upstream.trim())
    if (target.host !== host) continue
    if (target.port !== null && target.port !== port) continue
    if (!best || rule.priority > best.priority) best = rule
  }
  return best
}

export function ruleDisplayName(rule: ClientChaosRule): string {
  return rule.name || rule.upstream
}
