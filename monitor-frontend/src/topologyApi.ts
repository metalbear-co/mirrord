export interface ActiveSession {
  user: string
  target: string
  target_type: string
}

export interface TopologyEdge {
  source: string
  targets: string[]
}

export interface TopologyResponse {
  edges: TopologyEdge[]
  sessions: ActiveSession[]
  updated_at: string
}

export async function fetchTopology(): Promise<TopologyResponse> {
  const response = await fetch('/api/topology', {
    headers: { Accept: 'application/json' },
  })

  if (!response.ok) {
    throw new Error(`Topology API returned ${response.status}`)
  }

  return response.json()
}
