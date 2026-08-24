import type { ClientChaosRule } from '../../types'

// Which rule owns an outgoing connection is decided by the proxy and carried on the
// event, rather than recomputed here. A selector is matched against the hostname the
// app asked for as well as the resolved address, and only the address reaches the
// browser, so any attempt to redo the match here would drift from `AddressFilter`.
// The proxy also picks the winner by priority, so there is nothing left to choose.
//
// Note this is targeting, not faulting: a rule with a percentage targets every
// connection it selects while faulting only a share of them, so more events can carry
// a rule than the rule's hit counter reports.
export function matchChaosRule(
  rules: ClientChaosRule[],
  targetedBy: string | null | undefined,
): ClientChaosRule | null {
  if (!targetedBy) return null
  return (
    rules.find((rule) => rule.armed && rule.serverId === targetedBy) ?? null
  )
}

export function ruleDisplayName(rule: ClientChaosRule): string {
  return rule.name || rule.upstream
}
