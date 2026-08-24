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

const IPV4 = /^\d{1,3}(?:\.\d{1,3}){3}$/

// A selector host is a literal address when it parses as IPv4, or carries the colons
// or brackets of an IPv6 literal. Anything else is treated as a name.
function isLiteralAddress(host: string): boolean {
  return IPV4.test(host) || host.includes(':') || host.startsWith('[')
}

// The backend doesn't say which individual connections a rule affected (only the
// aggregate hit counter), so the stream tags every outgoing event a rule *targets*:
// the highest-priority armed rule whose host (and port, when given) matches.
//
// Matching mirrors how the proxy reads a selector. A literal address is compared to
// the address the connection resolved to. A name is matched as a substring of the
// hostname the app asked for, which is how a bare service name also matches its
// fully qualified form. The two are kept apart deliberately: matching a literal
// address against the hostname would attribute a connection to a rule that never
// targeted it, whenever a hostname happens to contain that address as text.
export function matchChaosRule(
  rules: ClientChaosRule[],
  address: string,
  port: number,
  hostname?: string | null,
): ClientChaosRule | null {
  const resolved = stripPortSuffix(address, port)
  let best: ClientChaosRule | null = null
  for (const rule of rules) {
    if (!rule.armed) continue
    const target = splitUpstream(rule.upstream.trim())
    // A selector renders as `host:port`, and a rule that named no port renders as
    // port 0, which the proxy reads as any port.
    if (target.port !== null && target.port !== 0 && target.port !== port)
      continue
    const matched = isLiteralAddress(target.host)
      ? target.host === resolved
      : Boolean(hostname?.includes(target.host))
    if (!matched) continue
    if (!best || rule.priority > best.priority) best = rule
  }
  return best
}

export function ruleDisplayName(rule: ClientChaosRule): string {
  return rule.name || rule.upstream
}
