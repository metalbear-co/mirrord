import {
  useCallback,
  useEffect,
  useMemo,
  useState,
  memo,
  type CSSProperties,
} from 'react'
import {
  Background,
  Controls,
  Handle,
  MarkerType,
  Position,
  ReactFlow,
  useEdgesState,
  useNodesState,
  type Edge,
  type Node,
  type NodeProps,
  type NodeMouseHandler,
} from '@xyflow/react'
import '@xyflow/react/dist/style.css'
import ELK from 'elkjs/lib/elk.bundled.js'
import { Laptop, Waypoints } from 'lucide-react'
import type { OperatorSessionSummary, SessionInfo } from '../../types'
import { strings } from '../../strings'
import {
  buildServiceGraph,
  type ServiceCategory,
  type ServiceGraphModel,
} from './serviceMapModel'

const elk = new ELK()

const ELK_OPTIONS = {
  'elk.algorithm': 'layered',
  'elk.direction': 'RIGHT',
  'elk.layered.spacing.nodeNodeBetweenLayers': '280',
  'elk.layered.spacing.nodeNode': '110',
  'elk.spacing.edgeNode': '60',
  'elk.spacing.edgeEdge': '30',
  'elk.padding': '[top=80,left=80,bottom=80,right=80]',
  'elk.edgeRouting': 'ORTHOGONAL',
  'elk.layered.crossingMinimization.strategy': 'LAYER_SWEEP',
  'elk.layered.nodePlacement.strategy': 'NETWORK_SIMPLEX',
  'elk.separateConnectedComponents': 'true',
}

const NODE_WIDTH = 220
const NODE_HEIGHT = 92
const DEV_NODE_WIDTH = 170
const DEV_NODE_HEIGHT = 58
const MIN_ZOOM = 0.2
const MAX_ZOOM = 2.5
const FIT_VIEW_PADDING = 0.1
const DIMMED_NODE_OPACITY = 0.2
const DIMMED_EDGE_OPACITY = 0.1
const EDGE_STROKE_WIDTH = 1.5
const HOVERED_EDGE_STROKE_WIDTH = 2.5
const BG_DOT_OPACITY_DARK = 0.2
const BG_DOT_OPACITY_LIGHT = 0.12

const PULSE_KEYFRAMES = `
  @keyframes mirrord-map-pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.4; }
  }
`

const CATEGORY_COLORS: Record<
  ServiceCategory,
  {
    bg: string
    border: string
    dot: string
    text: string
    bgDark: string
    borderDark: string
    textDark: string
  }
> = {
  entry: {
    bg: '#fef3c7',
    border: '#f59e0b',
    dot: '#f59e0b',
    text: '#92400e',
    bgDark: '#422006',
    borderDark: '#d97706',
    textDark: '#fde68a',
  },
  core: {
    bg: '#fee2e2',
    border: '#ef4444',
    dot: '#ef4444',
    text: '#991b1b',
    bgDark: '#450a0a',
    borderDark: '#dc2626',
    textDark: '#fecaca',
  },
  data: {
    bg: '#dcfce7',
    border: '#22c55e',
    dot: '#22c55e',
    text: '#166534',
    bgDark: '#052e16',
    borderDark: '#16a34a',
    textDark: '#bbf7d0',
  },
  queue: {
    bg: '#ffedd5',
    border: '#f97316',
    dot: '#f97316',
    text: '#9a3412',
    bgDark: '#431407',
    borderDark: '#ea580c',
    textDark: '#fed7aa',
  },
  infra: {
    bg: '#f1f5f9',
    border: '#94a3b8',
    dot: '#64748b',
    text: '#334155',
    bgDark: '#1e293b',
    borderDark: '#475569',
    textDark: '#cbd5e1',
  },
}

const DEV_COLOR = '#818cf8'

function categorySubtitle(
  category: ServiceCategory,
  discovered: boolean,
): string {
  if (discovered) return strings.map.discovered
  switch (category) {
    case 'data':
      return strings.map.kindData
    case 'queue':
      return strings.map.kindQueue
    case 'entry':
      return strings.map.kindEntry
    case 'infra':
      return strings.map.kindInfra
    case 'core':
      return strings.map.kindService
  }
}

function serviceNodeStyle(
  category: ServiceCategory,
  dark: boolean,
): CSSProperties {
  const colors = CATEGORY_COLORS[category]
  return {
    background: dark ? colors.bgDark : colors.bg,
    border: `2px solid ${dark ? colors.borderDark : colors.border}`,
    borderRadius: 12,
    padding: '12px 16px',
    color: dark ? colors.textDark : colors.text,
    fontSize: 13,
    fontWeight: 600,
    width: NODE_WIDTH,
    textAlign: 'left' as const,
    boxShadow: dark
      ? '0 2px 8px rgba(0,0,0,0.3)'
      : '0 2px 8px rgba(0, 0, 0, 0.06)',
    lineHeight: 1.4,
    transition: 'opacity 0.2s, box-shadow 0.2s',
  }
}

function devNodeStyle(dark: boolean): CSSProperties {
  return {
    background: dark ? '#1e1b4b' : '#eef2ff',
    border: `2px dashed ${DEV_COLOR}`,
    borderRadius: 12,
    padding: '10px 14px',
    color: dark ? '#c7d2fe' : '#312e81',
    fontSize: 13,
    fontWeight: 600,
    width: DEV_NODE_WIDTH,
    textAlign: 'left' as const,
    boxShadow: dark
      ? '0 2px 8px rgba(99,102,241,0.2)'
      : '0 2px 8px rgba(99, 102, 241, 0.12)',
    transition: 'opacity 0.2s, box-shadow 0.2s',
  }
}

const ServiceNode = memo(({ data }: NodeProps) => {
  const category = (data['category'] ?? 'core') as ServiceCategory
  const discovered = Boolean(data['discovered'])
  const sessions = (data['sessions'] ?? 0) as number
  const namespace = data['namespace'] as string
  const colors = CATEGORY_COLORS[category]
  return (
    <>
      <Handle
        type="target"
        position={Position.Left}
        style={{ opacity: 0, width: 1, height: 1 }}
      />
      <div style={{ display: 'flex', alignItems: 'flex-start', gap: 10 }}>
        <div
          style={{
            width: 10,
            height: 10,
            borderRadius: '50%',
            background: colors.dot,
            marginTop: 4,
            flexShrink: 0,
            boxShadow: `0 0 6px ${colors.dot}40`,
          }}
        />
        <div style={{ minWidth: 0 }}>
          <div
            style={{
              fontSize: 15,
              fontWeight: 700,
              marginBottom: 2,
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
            }}
          >
            {data['label'] as string}
          </div>
          <div
            style={{
              fontSize: 10,
              letterSpacing: '0.06em',
              opacity: 0.5,
              textTransform: 'uppercase',
              marginBottom: 5,
            }}
          >
            {categorySubtitle(category, discovered)}
          </div>
          <div style={{ fontSize: 10, opacity: 0.65 }}>{namespace}</div>
          {sessions > 0 && (
            <div
              style={{
                fontSize: 10,
                color: DEV_COLOR,
                fontWeight: 600,
                marginTop: 3,
                display: 'flex',
                alignItems: 'center',
                gap: 4,
              }}
            >
              <span
                style={{
                  width: 6,
                  height: 6,
                  borderRadius: '50%',
                  background: DEV_COLOR,
                  display: 'inline-block',
                  animation: 'mirrord-map-pulse 2s infinite',
                }}
              />
              {sessions === 1
                ? strings.map.oneSession
                : strings.map.manySessions.replace('{count}', String(sessions))}
            </div>
          )}
        </div>
      </div>
      <Handle
        type="source"
        position={Position.Right}
        style={{ opacity: 0, width: 1, height: 1 }}
      />
    </>
  )
})

ServiceNode.displayName = 'ServiceNode'

const DevNode = memo(({ data }: NodeProps) => {
  const dark = Boolean(data['dark'])
  return (
    <>
      <Handle
        type="target"
        position={Position.Left}
        style={{ opacity: 0, width: 1, height: 1 }}
      />
      <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
        <Laptop
          style={{ width: 18, height: 18, color: DEV_COLOR, flexShrink: 0 }}
        />
        <div style={{ minWidth: 0 }}>
          <div
            style={{
              fontSize: 13,
              fontWeight: 700,
              color: dark ? '#c7d2fe' : '#312e81',
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
            }}
          >
            {data['label'] as string}
          </div>
          <div
            style={{
              fontSize: 10,
              letterSpacing: '0.06em',
              opacity: 0.5,
              textTransform: 'uppercase',
              color: dark ? '#a5b4fc' : '#4338ca',
            }}
          >
            {data['isLocal'] ? strings.map.localMachine : strings.map.teammate}
          </div>
        </div>
      </div>
      <Handle
        type="source"
        position={Position.Right}
        style={{ opacity: 0, width: 1, height: 1 }}
      />
    </>
  )
})

DevNode.displayName = 'DevNode'

const nodeTypes = { service: ServiceNode, dev: DevNode }

function toFlowGraph(
  graph: ServiceGraphModel,
  dark: boolean,
): { nodes: Node[]; edges: Edge[] } {
  const nodes: Node[] = []
  for (const service of graph.services) {
    nodes.push({
      id: service.id,
      type: 'service',
      data: {
        label: service.name,
        category: service.category,
        namespace: service.namespace,
        sessions: service.sessions,
        discovered: service.discovered,
        dark,
      },
      position: { x: 0, y: 0 },
      style: serviceNodeStyle(service.category, dark),
    })
  }
  for (const dev of graph.devs) {
    nodes.push({
      id: dev.id,
      type: 'dev',
      data: { label: dev.label, isLocal: dev.isLocal, dark },
      position: { x: 0, y: 0 },
      style: devNodeStyle(dark),
    })
  }

  const trafficColor = dark ? '#475569' : '#94a3b8'
  const edges: Edge[] = graph.edges.map((edge) => {
    const isSession = edge.kind === 'session'
    const color = isSession ? DEV_COLOR : trafficColor
    return {
      id: edge.id,
      source: edge.source,
      target: edge.target,
      type: 'smoothstep',
      animated: isSession,
      style: {
        stroke: color,
        strokeWidth: 1.5,
        ...(isSession && { strokeDasharray: '6 3' }),
      },
      markerEnd: {
        type: MarkerType.ArrowClosed,
        color,
        width: 18,
        height: 18,
      },
    }
  })

  return { nodes, edges }
}

async function layoutGraph(nodes: Node[], edges: Edge[]): Promise<Node[]> {
  const layout = await elk.layout({
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
  })
  return nodes.map((node) => {
    const laid = layout.children?.find((c) => c.id === node.id)
    return { ...node, position: { x: laid?.x ?? 0, y: laid?.y ?? 0 } }
  })
}

const LEGEND: { label: string; color: string }[] = [
  { label: strings.map.legendEntry, color: CATEGORY_COLORS.entry.dot },
  { label: strings.map.legendCore, color: CATEGORY_COLORS.core.dot },
  { label: strings.map.legendData, color: CATEGORY_COLORS.data.dot },
  { label: strings.map.legendQueue, color: CATEGORY_COLORS.queue.dot },
  { label: strings.map.legendDev, color: DEV_COLOR },
]

function Legend({ dark }: { dark: boolean }) {
  return (
    <div
      style={{
        position: 'absolute',
        top: 16,
        right: 16,
        zIndex: 10,
        background: dark ? 'rgba(15,23,42,0.95)' : 'rgba(255,255,255,0.95)',
        border: `1px solid ${dark ? '#334155' : '#e2e8f0'}`,
        borderRadius: 10,
        padding: '12px 16px',
        fontSize: 12,
        boxShadow: dark
          ? '0 2px 8px rgba(0,0,0,0.3)'
          : '0 2px 8px rgba(0,0,0,0.06)',
        backdropFilter: 'blur(8px)',
      }}
    >
      <div
        style={{
          fontWeight: 700,
          marginBottom: 8,
          fontSize: 11,
          textTransform: 'uppercase',
          letterSpacing: '0.06em',
          color: dark ? '#94a3b8' : '#64748b',
        }}
      >
        {strings.map.legendTitle}
      </div>
      {LEGEND.map(({ label, color }) => (
        <div
          key={label}
          style={{
            display: 'flex',
            alignItems: 'center',
            gap: 10,
            marginBottom: 5,
          }}
        >
          <div
            style={{
              width: 8,
              height: 8,
              borderRadius: '50%',
              background: color,
              boxShadow: `0 0 4px ${color}40`,
            }}
          />
          <span style={{ color: dark ? '#cbd5e1' : '#475569' }}>{label}</span>
        </div>
      ))}
    </div>
  )
}

function graphSignature(graph: ServiceGraphModel, dark: boolean): string {
  return JSON.stringify([
    graph.services.map((s) => [s.id, s.category, s.sessions, s.discovered]),
    graph.devs.map((d) => d.id),
    graph.edges.map((e) => e.id),
    dark,
  ])
}

function MapCanvas({
  initialNodes,
  initialEdges,
  dark,
}: {
  initialNodes: Node[]
  initialEdges: Edge[]
  dark: boolean
}) {
  const [nodes, setNodes, onNodesChange] = useNodesState<Node>(initialNodes)
  const [edges, setEdges, onEdgesChange] = useEdgesState(initialEdges)
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
      style: { ...node.style, opacity: connected ? 1 : DIMMED_NODE_OPACITY },
    }
  })

  const styledEdges = edges.map((edge) => {
    if (!hoveredNode) return edge
    const connected = edge.source === hoveredNode || edge.target === hoveredNode
    return {
      ...edge,
      style: {
        ...edge.style,
        opacity: connected ? 1 : DIMMED_EDGE_OPACITY,
        strokeWidth: connected ? HOVERED_EDGE_STROKE_WIDTH : EDGE_STROKE_WIDTH,
      },
    }
  })

  return (
    <ReactFlow
      nodes={styledNodes}
      edges={styledEdges}
      nodeTypes={nodeTypes}
      onNodesChange={onNodesChange}
      onEdgesChange={onEdgesChange}
      onNodeMouseEnter={onNodeMouseEnter}
      onNodeMouseLeave={onNodeMouseLeave}
      fitView
      fitViewOptions={{ padding: FIT_VIEW_PADDING }}
      proOptions={{ hideAttribution: true }}
      minZoom={MIN_ZOOM}
      maxZoom={MAX_ZOOM}
    >
      <Background
        gap={24}
        size={1}
        color={dark ? '#334155' : '#cbd5e1'}
        style={{ opacity: dark ? BG_DOT_OPACITY_DARK : BG_DOT_OPACITY_LIGHT }}
      />
      <Controls
        style={
          {
            border: `1px solid ${dark ? '#334155' : '#e2e8f0'}`,
            borderRadius: 8,
            overflow: 'hidden',
            '--xy-controls-button-background-color': dark
              ? '#1e293b'
              : '#ffffff',
            '--xy-controls-button-background-color-hover': dark
              ? '#334155'
              : '#f8fafc',
            '--xy-controls-button-color': dark ? '#cbd5e1' : '#334155',
            '--xy-controls-button-border-color': dark ? '#334155' : '#e2e8f0',
          } as CSSProperties
        }
      />
    </ReactFlow>
  )
}

interface Props {
  localSessions: SessionInfo[]
  operatorSessions: OperatorSessionSummary[]
  trafficBySession: Record<string, string[]>
  isDarkMode: boolean
}

export default function ServiceMapView({
  localSessions,
  operatorSessions,
  trafficBySession,
  isDarkMode,
}: Props) {
  const graph = useMemo(
    () =>
      buildServiceGraph({
        localSessions,
        operatorSessions,
        trafficBySession,
      }),
    [localSessions, operatorSessions, trafficBySession],
  )

  const signature = graphSignature(graph, isDarkMode)
  const [laidOut, setLaidOut] = useState<{
    signature: string
    nodes: Node[]
    edges: Edge[]
  } | null>(null)

  useEffect(() => {
    let cancelled = false
    const { nodes, edges } = toFlowGraph(graph, isDarkMode)
    if (nodes.length === 0) {
      setLaidOut({ signature, nodes: [], edges: [] })
      return
    }
    layoutGraph(nodes, edges)
      .then((positioned) => {
        if (!cancelled) setLaidOut({ signature, nodes: positioned, edges })
      })
      .catch((err: unknown) => console.error(err))
    return () => {
      cancelled = true
    }
  }, [signature, graph, isDarkMode])

  const empty = graph.services.length === 0 && graph.devs.length === 0
  const current = laidOut?.signature === signature ? laidOut : null

  return (
    <div className="relative h-full w-full">
      <style>{PULSE_KEYFRAMES}</style>
      {empty ? (
        <div className="text-muted-foreground flex h-full flex-col items-center justify-center gap-2 px-8 text-center">
          <Waypoints className="mb-2 h-8 w-8 opacity-50" />
          <p className="text-foreground text-lg font-medium">
            {strings.map.emptyTitle}
          </p>
          <p className="max-w-md text-sm opacity-80">{strings.map.emptyBody}</p>
        </div>
      ) : current ? (
        <>
          <Legend dark={isDarkMode} />
          <MapCanvas
            initialNodes={current.nodes}
            initialEdges={current.edges}
            dark={isDarkMode}
          />
        </>
      ) : (
        <div className="text-muted-foreground flex h-full items-center justify-center">
          {strings.map.loading}
        </div>
      )}
    </div>
  )
}
