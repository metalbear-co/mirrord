import { useEffect, useState, useCallback, memo, type CSSProperties } from 'react'
import {
  ReactFlow,
  Background,
  Controls,
  Handle,
  Position,
  MarkerType,
  useNodesState,
  useEdgesState,
  type Node,
  type Edge,
  type NodeProps,
  type NodeMouseHandler,
} from '@xyflow/react'
import '@xyflow/react/dist/style.css'
import ELK from 'elkjs/lib/elk.bundled.js'
import { fetchTopology, type TopologyResponse, type TopologyEdge, type ActiveSession } from './topologyApi'
import { MirrordIcon } from '@metalbear/ui'
import { Laptop, Network, Users, Shield, Zap, ArrowRight } from 'lucide-react'

const elk = new ELK()

const ELK_OPTIONS = {
  'elk.algorithm': 'layered',
  'elk.direction': 'RIGHT',
  'elk.layered.spacing.nodeNodeBetweenLayers': '400',
  'elk.layered.spacing.nodeNode': '140',
  'elk.spacing.edgeNode': '80',
  'elk.spacing.edgeEdge': '30',
  'elk.padding': '[top=100,left=100,bottom=100,right=100]',
  'elk.edgeRouting': 'ORTHOGONAL',
  'elk.layered.crossingMinimization.strategy': 'LAYER_SWEEP',
  'elk.layered.crossingMinimization.greedySwitch.type': 'TWO_SIDED',
  'elk.layered.crossingMinimization.semiInteractive': 'true',
  'elk.layered.nodePlacement.strategy': 'NETWORK_SIMPLEX',
  'elk.layered.thoroughness': '200',
  'elk.layered.wrapping.strategy': 'OFF',
  'elk.separateConnectedComponents': 'false',
}

const NODE_WIDTH = 230
const NODE_HEIGHT = 95
const DEV_NODE_WIDTH = 180
const DEV_NODE_HEIGHT = 60

type ServiceCategory = 'entry' | 'core' | 'data' | 'queue' | 'infra'

const CATEGORY_COLORS: Record<ServiceCategory, { bg: string; border: string; dot: string; text: string; bgDark: string; borderDark: string; textDark: string }> = {
  entry: { bg: '#fef3c7', border: '#f59e0b', dot: '#f59e0b', text: '#92400e', bgDark: '#422006', borderDark: '#d97706', textDark: '#fde68a' },
  core:  { bg: '#fee2e2', border: '#ef4444', dot: '#ef4444', text: '#991b1b', bgDark: '#450a0a', borderDark: '#dc2626', textDark: '#fecaca' },
  data:  { bg: '#dcfce7', border: '#22c55e', dot: '#22c55e', text: '#166534', bgDark: '#052e16', borderDark: '#16a34a', textDark: '#bbf7d0' },
  queue: { bg: '#ffedd5', border: '#f97316', dot: '#f97316', text: '#9a3412', bgDark: '#431407', borderDark: '#ea580c', textDark: '#fed7aa' },
  infra: { bg: '#f1f5f9', border: '#94a3b8', dot: '#64748b', text: '#334155', bgDark: '#1e293b', borderDark: '#475569', textDark: '#cbd5e1' },
}

const DATA_STORE_PATTERNS = ['postgres', 'mysql', 'mongo', 'redis', 'sqlite', 'dynamodb', 'firestore', 'elastic', 'memcached']
const QUEUE_PATTERNS = ['kafka', 'rabbitmq', 'sqs', 'nats', 'pubsub', 'queue', 'stream']
const ENTRY_PATTERNS = ['frontend', 'ingress', 'gateway', 'proxy', 'nginx', 'envoy', 'external', 'loadbalancer']

function categorize(name: string): ServiceCategory {
  const lower = name.toLowerCase()
  if (DATA_STORE_PATTERNS.some((p) => lower.includes(p))) return 'data'
  if (QUEUE_PATTERNS.some((p) => lower.includes(p))) return 'queue'
  if (ENTRY_PATTERNS.some((p) => lower.includes(p))) return 'entry'
  return 'core'
}

function isDark(): boolean {
  return document.documentElement.classList.contains('dark')
}

function nodeStyle(category: ServiceCategory): CSSProperties {
  const colors = CATEGORY_COLORS[category]
  const dark = isDark()
  return {
    background: dark ? colors.bgDark : colors.bg,
    border: `2px solid ${dark ? colors.borderDark : colors.border}`,
    borderRadius: 12,
    padding: '14px 18px',
    color: dark ? colors.textDark : colors.text,
    fontSize: 13,
    fontWeight: 600,
    fontFamily: 'ui-sans-serif, system-ui, -apple-system, sans-serif',
    width: NODE_WIDTH,
    textAlign: 'left' as const,
    boxShadow: dark ? '0 2px 8px rgba(0,0,0,0.3)' : '0 2px 8px rgba(0, 0, 0, 0.06)',
    lineHeight: 1.4,
    transition: 'opacity 0.2s, box-shadow 0.2s',
  }
}

function categorySubtitle(category: ServiceCategory): string {
  switch (category) {
    case 'data': return 'DATA STORE'
    case 'queue': return 'QUEUE / STREAM'
    case 'entry': return 'ENTRY POINT'
    case 'infra': return 'INFRASTRUCTURE'
    default: return 'SERVICE'
  }
}

const ServiceNode = memo(({ data }: NodeProps) => {
  const category = (data.category ?? 'core') as ServiceCategory
  const colors = CATEGORY_COLORS[category]
  const dark = isDark()
  const inbound = (data.inbound ?? 0) as number
  const outbound = (data.outbound ?? 0) as number
  const mirroredBy = (data.mirroredBy ?? 0) as number
  const textColor = dark ? colors.textDark : colors.text
  return (
    <>
      <Handle type="target" position={Position.Left} style={{ opacity: 0, width: 1, height: 1 }} />
      <div style={{ display: 'flex', alignItems: 'flex-start', gap: 10 }}>
        <div style={{
          width: 10,
          height: 10,
          borderRadius: '50%',
          background: colors.dot,
          marginTop: 4,
          flexShrink: 0,
          boxShadow: `0 0 6px ${colors.dot}40`,
        }} />
        <div style={{ minWidth: 0 }}>
          <div style={{ fontSize: 15, fontWeight: 700, marginBottom: 2 }}>
            {data.label as string}
          </div>
          <div style={{ fontSize: 10, letterSpacing: '0.06em', opacity: 0.5, textTransform: 'uppercase', marginBottom: 5 }}>
            {categorySubtitle(category)}
          </div>
          <div style={{ fontSize: 10, color: textColor, opacity: 0.65, lineHeight: 1.5, display: 'flex', gap: 6 }}>
            {inbound > 0 && (
              <span style={{ display: 'inline-flex', alignItems: 'center', gap: 2 }}>
                <span style={{ fontSize: 8 }}>&#9664;</span> {inbound} in
              </span>
            )}
            {outbound > 0 && (
              <span style={{ display: 'inline-flex', alignItems: 'center', gap: 2 }}>
                {outbound} out <span style={{ fontSize: 8 }}>&#9654;</span>
              </span>
            )}
          </div>
          {mirroredBy > 0 && (
            <div style={{
              fontSize: 10,
              color: '#818cf8',
              fontWeight: 600,
              marginTop: 3,
              display: 'flex',
              alignItems: 'center',
              gap: 4,
            }}>
              <span style={{ width: 6, height: 6, borderRadius: '50%', background: '#818cf8', display: 'inline-block', animation: 'pulse 2s infinite' }} />
              {mirroredBy} {mirroredBy === 1 ? 'dev' : 'devs'} mirroring
            </div>
          )}
        </div>
      </div>
      <Handle type="source" position={Position.Right} style={{ opacity: 0, width: 1, height: 1 }} />
    </>
  )
})

const DevNode = memo(({ data }: NodeProps) => {
  const dark = isDark()
  return (
    <>
      <Handle type="target" position={Position.Left} style={{ opacity: 0, width: 1, height: 1 }} />
      <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
        <Laptop style={{ width: 18, height: 18, color: '#818cf8', flexShrink: 0 }} />
        <div>
          <div style={{ fontSize: 14, fontWeight: 700, color: dark ? '#c7d2fe' : '#312e81' }}>
            {data.label as string}
          </div>
          <div style={{ fontSize: 10, letterSpacing: '0.06em', opacity: 0.5, textTransform: 'uppercase', color: dark ? '#a5b4fc' : '#4338ca' }}>
            LOCAL MACHINE
          </div>
        </div>
      </div>
      <Handle type="source" position={Position.Right} style={{ opacity: 0, width: 1, height: 1 }} />
    </>
  )
})

const nodeTypes = { service: ServiceNode, dev: DevNode }

async function layoutGraph(
  nodes: Node[],
  edges: Edge[],
): Promise<{ nodes: Node[]; edges: Edge[] }> {
  const graph = {
    id: 'root',
    layoutOptions: ELK_OPTIONS,
    children: nodes.map((n) => ({
      id: n.id,
      width: n.type === 'dev' ? DEV_NODE_WIDTH : NODE_WIDTH,
      height: n.type === 'dev' ? DEV_NODE_HEIGHT : NODE_HEIGHT,
    })),
    edges: edges.map((e) => ({
      id: e.id,
      sources: [e.source],
      targets: [e.target],
    })),
  }

  const layout = await elk.layout(graph)

  const positioned = nodes.map((node) => {
    const laid = layout.children?.find((c) => c.id === node.id)
    return {
      ...node,
      position: { x: laid?.x ?? 0, y: laid?.y ?? 0 },
    }
  })

  return { nodes: positioned, edges }
}

function buildNodesAndEdges(data: TopologyResponse): {
  nodes: Node[]
  edges: Edge[]
} {
  const allServices = new Set<string>()

  for (const edge of data.edges) {
    allServices.add(edge.source)
    for (const dest of edge.targets) {
      allServices.add(dest)
    }
  }

  const outboundCount = new Map<string, number>()
  const inboundCount = new Map<string, number>()
  for (const edge of data.edges) {
    outboundCount.set(edge.source, (outboundCount.get(edge.source) ?? 0) + edge.targets.length)
    for (const dest of edge.targets) {
      inboundCount.set(dest, (inboundCount.get(dest) ?? 0) + 1)
    }
  }

  const mirrorCount = new Map<string, number>()
  for (const session of data.sessions ?? []) {
    mirrorCount.set(session.target, (mirrorCount.get(session.target) ?? 0) + 1)
  }

  const dark = isDark()

  const nodes: Node[] = Array.from(allServices).map((service) => {
    const category = categorize(service)
    return {
      id: service,
      type: 'service',
      data: {
        label: service,
        category,
        inbound: inboundCount.get(service) ?? 0,
        outbound: outboundCount.get(service) ?? 0,
        mirroredBy: mirrorCount.get(service) ?? 0,
      },
      position: { x: 0, y: 0 },
      style: nodeStyle(category),
    }
  })

  const edgeColor = dark ? '#475569' : '#94a3b8'
  const edges: Edge[] = []
  for (const edge of data.edges) {
    for (const dest of edge.targets) {
      edges.push({
        id: `${edge.source}-${dest}`,
        source: edge.source,
        target: dest,
        type: 'smoothstep',
        style: { stroke: edgeColor, strokeWidth: 1.5 },
        animated: false,
        markerEnd: {
          type: MarkerType.ArrowClosed,
          color: edgeColor,
          width: 18,
          height: 18,
        },
      })
    }
  }

  const sessionsByTarget = new Map<string, ActiveSession[]>()
  for (const session of data.sessions ?? []) {
    const list = sessionsByTarget.get(session.target) ?? []
    list.push(session)
    sessionsByTarget.set(session.target, list)
  }

  for (const [target, sessions] of sessionsByTarget) {
    for (const session of sessions) {
      const devId = `dev-${session.user.replace(/\s+/g, '-').toLowerCase()}`
      const firstName = session.user.split(/[\s@]/)[0] ?? session.user
      if (!nodes.find((n) => n.id === devId)) {
        nodes.push({
          id: devId,
          type: 'dev',
          data: { label: firstName },
          position: { x: 0, y: 0 },
          style: {
            background: dark ? '#1e1b4b' : '#eef2ff',
            border: '2px dashed #818cf8',
            borderRadius: 12,
            padding: '10px 16px',
            color: dark ? '#c7d2fe' : '#312e81',
            fontSize: 13,
            fontWeight: 600,
            fontFamily: 'ui-sans-serif, system-ui, -apple-system, sans-serif',
            width: DEV_NODE_WIDTH,
            textAlign: 'left' as const,
            boxShadow: dark ? '0 2px 8px rgba(99,102,241,0.2)' : '0 2px 8px rgba(99, 102, 241, 0.12)',
            transition: 'opacity 0.2s, box-shadow 0.2s',
          },
        })
      }
      edges.push({
        id: `${devId}-${target}`,
        source: devId,
        target,
        type: 'smoothstep',
        style: { stroke: '#818cf8', strokeWidth: 1.5, strokeDasharray: '6 3' },
        animated: true,
        markerEnd: {
          type: MarkerType.ArrowClosed,
          color: '#818cf8',
          width: 18,
          height: 18,
        },
      })
    }
  }

  return { nodes, edges }
}

const LEGEND: { label: string; color: string }[] = [
  { label: 'Entry / Client', color: CATEGORY_COLORS.entry.dot },
  { label: 'Core Services', color: CATEGORY_COLORS.core.dot },
  { label: 'Data Stores', color: CATEGORY_COLORS.data.dot },
  { label: 'Queues & Streams', color: CATEGORY_COLORS.queue.dot },
  { label: 'Infrastructure', color: CATEGORY_COLORS.infra.dot },
  { label: 'Active Developer', color: '#818cf8' },
]

function Legend() {
  const dark = isDark()
  return (
    <div style={{
      position: 'absolute',
      top: 16,
      left: 16,
      zIndex: 10,
      background: dark ? 'rgba(15,23,42,0.95)' : 'rgba(255,255,255,0.95)',
      border: `1px solid ${dark ? '#334155' : '#e2e8f0'}`,
      borderRadius: 10,
      padding: '14px 18px',
      fontSize: 12,
      fontFamily: 'ui-sans-serif, system-ui, sans-serif',
      boxShadow: dark ? '0 2px 8px rgba(0,0,0,0.3)' : '0 2px 8px rgba(0,0,0,0.06)',
      backdropFilter: 'blur(8px)',
    }}>
      <div style={{ fontWeight: 700, marginBottom: 10, fontSize: 11, textTransform: 'uppercase', letterSpacing: '0.06em', color: dark ? '#94a3b8' : '#64748b' }}>
        Legend
      </div>
      {LEGEND.map(({ label, color }) => (
        <div key={label} style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 5 }}>
          <div style={{
            width: 8,
            height: 8,
            borderRadius: '50%',
            background: color,
            boxShadow: `0 0 4px ${color}40`,
          }} />
          <span style={{ color: dark ? '#cbd5e1' : '#475569' }}>{label}</span>
        </div>
      ))}
    </div>
  )
}

function TopologyInner({ initialNodes, initialEdges }: { initialNodes: Node[]; initialEdges: Edge[] }) {
  const [nodes, setNodes, onNodesChange] = useNodesState<Node>(initialNodes)
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>(initialEdges)
  const [hoveredNode, setHoveredNode] = useState<string | null>(null)

  useEffect(() => {
    setNodes(initialNodes)
    setEdges(initialEdges)
  }, [initialNodes, initialEdges, setNodes, setEdges])

  const onNodeMouseEnter: NodeMouseHandler = useCallback((_event, node) => {
    setHoveredNode(node.id)
  }, [])

  const onNodeMouseLeave = useCallback(() => {
    setHoveredNode(null)
  }, [])

  const styledNodes = nodes.map((node) => {
    if (!hoveredNode) return node
    const connected =
      node.id === hoveredNode ||
      edges.some(
        (e) =>
          (e.source === hoveredNode && e.target === node.id) ||
          (e.target === hoveredNode && e.source === node.id),
      )
    return {
      ...node,
      style: {
        ...node.style,
        opacity: connected ? 1 : 0.2,
        boxShadow: connected && node.id === hoveredNode
          ? `0 0 0 3px ${CATEGORY_COLORS[(node.data?.category as ServiceCategory) ?? 'core']?.dot ?? '#818cf8'}40, ${String((node.style as Record<string, unknown>)?.boxShadow ?? '')}`
          : String((node.style as Record<string, unknown>)?.boxShadow ?? ''),
      },
    }
  })

  const styledEdges = edges.map((edge) => {
    if (!hoveredNode) return edge
    const connected = edge.source === hoveredNode || edge.target === hoveredNode
    return {
      ...edge,
      style: {
        ...edge.style,
        opacity: connected ? 1 : 0.1,
        strokeWidth: connected ? 2.5 : 1.5,
      },
    }
  })

  const dark = isDark()

  return (
    <div style={{
      height: '100%',
      overflow: 'hidden',
      position: 'relative',
      background: dark ? '#0f172a' : '#fafbfc',
    }}>
      <style>{`
        @keyframes pulse {
          0%, 100% { opacity: 1; }
          50% { opacity: 0.4; }
        }
      `}</style>
      <Legend />
      <ReactFlow
        nodes={styledNodes}
        edges={styledEdges}
        nodeTypes={nodeTypes}
        onNodesChange={onNodesChange}
        onEdgesChange={onEdgesChange}
        onNodeMouseEnter={onNodeMouseEnter}
        onNodeMouseLeave={onNodeMouseLeave}
        fitView
        fitViewOptions={{ padding: 0.08 }}
        proOptions={{ hideAttribution: true }}
        minZoom={0.2}
        maxZoom={2.5}
      >
        <Background
          gap={24}
          size={1}
          color={dark ? '#334155' : '#cbd5e1'}
          style={{ opacity: dark ? 0.2 : 0.12 }}
        />
        <Controls
          style={{
            background: dark ? '#1e293b' : '#fff',
            border: `1px solid ${dark ? '#334155' : '#e2e8f0'}`,
            borderRadius: 8,
          }}
        />
      </ReactFlow>
    </div>
  )
}

function TopologyTeaser() {
  const features = [
    {
      icon: Network,
      title: 'Service Topology Map',
      description: 'Automatically discover and visualize how your services communicate in real time.',
    },
    {
      icon: Users,
      title: 'Team Collaboration',
      description: 'See who on your team is mirroring which services, with live developer nodes on the graph.',
    },
    {
      icon: Shield,
      title: 'Policy & Access Control',
      description: 'Fine-grained RBAC policies for who can mirror, steal, or access specific targets.',
    },
    {
      icon: Zap,
      title: 'Queue Splitting & More',
      description: 'Split Kafka and SQS queues, branch databases, and preview environments for your team.',
    },
  ]

  return (
    <div className="h-full overflow-auto">
      <div className="max-w-2xl mx-auto px-6 py-16 flex flex-col items-center">
        {/* Hero */}
        <div className="mb-8">
          <div className="w-20 h-20 rounded-2xl flex items-center justify-center bg-indigo-50 dark:bg-indigo-950 shadow-[0_0_40px_rgba(99,102,241,0.1)] dark:shadow-[0_0_40px_rgba(99,102,241,0.15)]">
            <img src={MirrordIcon} alt="mirrord" className="w-12 h-12 dark:invert" />
          </div>
        </div>

        <h2 className="text-2xl font-bold mb-3 text-center text-slate-800 dark:text-slate-100">
          See how your services connect
        </h2>
        <p className="text-center mb-10 leading-relaxed max-w-md text-[15px] text-slate-500 dark:text-slate-400">
          Service Topology automatically maps your cluster's service-to-service
          communication and shows who on your team is actively mirroring.
        </p>

        {/* Feature grid */}
        <div className="grid grid-cols-2 gap-4 w-full mb-10">
          {features.map(({ icon: Icon, title, description }) => (
            <div
              key={title}
              className="rounded-xl p-4 bg-slate-50 dark:bg-slate-900 border border-slate-200 dark:border-slate-800"
            >
              <Icon className="w-5 h-5 mb-2.5 text-indigo-500 dark:text-indigo-400" />
              <div className="text-sm font-semibold mb-1 text-slate-700 dark:text-slate-200">
                {title}
              </div>
              <div className="text-xs leading-relaxed text-slate-400 dark:text-slate-500">
                {description}
              </div>
            </div>
          ))}
        </div>

        {/* CTA */}
        <a
          href="https://app.metalbear.com/account/sign-up"
          target="_blank"
          rel="noopener noreferrer"
          className="inline-flex items-center gap-2 px-6 py-3 rounded-lg text-sm font-semibold text-white transition-all hover:scale-[1.02] active:scale-[0.98] bg-[linear-gradient(135deg,#7c5aff_0%,#6366f1_100%)] shadow-[0_4px_16px_rgba(124,90,255,0.3)]"
        >
          Try mirrord for Teams
          <ArrowRight className="w-4 h-4" />
        </a>

        <p className="mt-4 text-xs text-center text-slate-400 dark:text-slate-600">
          Free to get started. Install the operator to unlock topology, queue splitting, and more.
        </p>
      </div>
    </div>
  )
}

export default function TopologyView() {
  const [layoutedNodes, setLayoutedNodes] = useState<Node[]>([])
  const [layoutedEdges, setLayoutedEdges] = useState<Edge[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const loadTopology = useCallback(async () => {
    try {
      const data = await fetchTopology()

      if (data.edges.length === 0) {
        setLayoutedNodes([])
        setLayoutedEdges([])
        setLoading(false)
        return
      }

      const { nodes: rawNodes, edges: rawEdges } = buildNodesAndEdges(data)
      const { nodes: positioned, edges: laid } = await layoutGraph(rawNodes, rawEdges)
      setLayoutedNodes(positioned)
      setLayoutedEdges(laid)
      setError(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load topology')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    loadTopology()
    const interval = setInterval(loadTopology, 10_000)
    return () => clearInterval(interval)
  }, [loadTopology])

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full text-muted-foreground">
        Loading topology...
      </div>
    )
  }

  if (error) {
    return <TopologyTeaser />
  }

  if (layoutedNodes.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground gap-2">
        <p className="text-lg">No service relationships discovered yet</p>
        <p className="text-sm opacity-70">
          Service topology builds automatically as your team uses mirrord.
          Start a session to begin mapping.
        </p>
      </div>
    )
  }

  return <TopologyInner initialNodes={layoutedNodes} initialEdges={layoutedEdges} />
}
