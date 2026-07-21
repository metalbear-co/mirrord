import type { OperatorSessionSummary, SessionInfo } from '../../types'

export type ServiceCategory = 'entry' | 'core' | 'data' | 'queue' | 'infra'

const DATA_STORE_PATTERNS = [
  'postgres',
  'mysql',
  'mongo',
  'redis',
  'sqlite',
  'dynamodb',
  'firestore',
  'elastic',
  'memcached',
  'cassandra',
  'clickhouse',
]

const QUEUE_PATTERNS = [
  'kafka',
  'rabbitmq',
  'sqs',
  'nats',
  'pubsub',
  'queue',
  'stream',
  'celery',
]

const ENTRY_PATTERNS = [
  'frontend',
  'ingress',
  'gateway',
  'proxy',
  'nginx',
  'envoy',
  'external',
  'loadbalancer',
  'web',
]

export function categorize(name: string): ServiceCategory {
  const lower = name.toLowerCase()
  if (DATA_STORE_PATTERNS.some((p) => lower.includes(p))) return 'data'
  if (QUEUE_PATTERNS.some((p) => lower.includes(p))) return 'queue'
  if (ENTRY_PATTERNS.some((p) => lower.includes(p))) return 'entry'
  return 'core'
}

const CLUSTER_SUFFIXES = ['.svc.cluster.local', '.cluster.local', '.svc']

const EXTERNAL_TLDS = new Set([
  'com',
  'net',
  'org',
  'io',
  'dev',
  'co',
  'ai',
  'app',
  'sh',
  'gg',
  'me',
  'us',
  'uk',
  'de',
  'fr',
  'jp',
  'cn',
  'in',
  'edu',
  'gov',
  'mil',
  'info',
  'biz',
  'xyz',
  'cloud',
  'online',
  'local',
  'localhost',
  'internal',
  'arpa',
])

export interface ResolvedService {
  name: string
  namespace: string
}

export function resolveDnsHost(
  host: string,
  fallbackNamespace: string,
): ResolvedService | null {
  let h = host.trim().toLowerCase().replace(/\.+$/, '')
  if (!h || h.includes(':') || h.includes('_')) return null
  if (/^[\d.]+$/.test(h)) return null
  for (const suffix of CLUSTER_SUFFIXES) {
    if (h.endsWith(suffix)) {
      h = h.slice(0, -suffix.length)
      const parts = h.split('.')
      const name = parts[0]
      if (!name) return null
      return { name, namespace: parts[1] ?? fallbackNamespace }
    }
  }
  const parts = h.split('.')
  const name = parts[0]
  if (!name) return null
  if (parts.length === 1) return { name, namespace: fallbackNamespace }
  if (parts.length === 2 && parts[1] && !EXTERNAL_TLDS.has(parts[1])) {
    return { name, namespace: parts[1] }
  }
  return null
}

export interface ParsedTarget {
  kind: string
  name: string
}

export function parseLocalTarget(target: string): ParsedTarget | null {
  const parts = target.split('/')
  const kind = parts[0]
  const name = parts[1]
  if (!kind || !name || kind === 'targetless') return null
  return { kind, name }
}

export interface ServiceNodeModel {
  id: string
  name: string
  namespace: string
  kind: string | null
  category: ServiceCategory
  sessions: number
  discovered: boolean
}

export interface DevNodeModel {
  id: string
  label: string
  isLocal: boolean
}

export interface MapEdgeModel {
  id: string
  source: string
  target: string
  kind: 'session' | 'traffic'
}

export interface ServiceGraphModel {
  services: ServiceNodeModel[]
  devs: DevNodeModel[]
  edges: MapEdgeModel[]
}

export function serviceNodeId(name: string, namespace: string): string {
  return `svc:${namespace}/${name}`
}

const DEFAULT_NAMESPACE = 'default'

export function buildServiceGraph(input: {
  localSessions: SessionInfo[]
  operatorSessions: OperatorSessionSummary[]
  trafficBySession: Record<string, string[]>
}): ServiceGraphModel {
  const services = new Map<string, ServiceNodeModel>()
  const devs = new Map<string, DevNodeModel>()
  const edges = new Map<string, MapEdgeModel>()

  const addService = (
    name: string,
    namespace: string,
    kind: string | null,
    discovered: boolean,
  ): ServiceNodeModel => {
    const id = serviceNodeId(name, namespace)
    const existing = services.get(id)
    if (existing) {
      if (!discovered && existing.discovered) {
        existing.discovered = false
        existing.kind = kind
      }
      return existing
    }
    const node: ServiceNodeModel = {
      id,
      name,
      namespace,
      kind,
      category: categorize(name),
      sessions: 0,
      discovered,
    }
    services.set(id, node)
    return node
  }

  const addEdge = (
    source: string,
    target: string,
    kind: 'session' | 'traffic',
  ) => {
    if (source === target) return
    const id = `${kind}:${source}->${target}`
    if (!edges.has(id)) edges.set(id, { id, source, target, kind })
  }

  const localSourceById = new Map<
    string,
    { nodeId: string; namespace: string }
  >()

  for (const session of input.localSessions) {
    const namespace = session.namespace ?? DEFAULT_NAMESPACE
    const target = parseLocalTarget(session.target)
    const devId = `dev:local:${session.session_id}`
    const label = session.processes[0]?.process_name ?? 'local process'
    devs.set(devId, { id: devId, label, isLocal: true })
    if (target) {
      const node = addService(target.name, namespace, target.kind, false)
      node.sessions += 1
      addEdge(devId, node.id, 'session')
      localSourceById.set(session.session_id, { nodeId: node.id, namespace })
    } else {
      localSourceById.set(session.session_id, { nodeId: devId, namespace })
    }
  }

  for (const session of input.operatorSessions) {
    const username = session.owner.k8sUsername || session.owner.username
    const devId = `dev:operator:${username}`
    const label = username.split(/[\s@]/)[0] ?? username
    if (!devs.has(devId)) devs.set(devId, { id: devId, label, isLocal: false })
    if (session.target) {
      const node = addService(
        session.target.name,
        session.namespace || DEFAULT_NAMESPACE,
        session.target.kind,
        false,
      )
      node.sessions += 1
      addEdge(devId, node.id, 'session')
    }
  }

  for (const [sessionId, hosts] of Object.entries(input.trafficBySession)) {
    const source = localSourceById.get(sessionId)
    if (!source) continue
    for (const host of hosts) {
      const resolved = resolveDnsHost(host, source.namespace)
      if (!resolved) continue
      const node = addService(resolved.name, resolved.namespace, null, true)
      addEdge(source.nodeId, node.id, 'traffic')
    }
  }

  return {
    services: [...services.values()].sort((a, b) => a.id.localeCompare(b.id)),
    devs: [...devs.values()].sort((a, b) => a.id.localeCompare(b.id)),
    edges: [...edges.values()].sort((a, b) => a.id.localeCompare(b.id)),
  }
}
