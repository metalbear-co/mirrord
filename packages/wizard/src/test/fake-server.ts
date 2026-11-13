import { vi } from 'vitest'
import type { ModeInfo, PickedService } from '../lib/up'

// Stands in for the `mirrord ui` server routes the wizard calls.

export const MODES: ModeInfo[] = [
  {
    id: 'split',
    label: 'Split',
    description:
      "Your requests reach your local process. Everyone else's keep hitting the deployed service, so you can work in a shared cluster without disrupting anyone.",
    recommended: true,
    target_types: ['deployment', 'statefulset', 'rollout', 'pod'],
    targetless: true,
    http_filter: true,
    traffic_question: 'Customize the header filter and ports?',
    traffic_default:
      'Default: `baggage: .*mirrord-session={key}.*` header filter on every port your app listens on.',
  },
  {
    id: 'mirror',
    label: 'Mirror',
    description:
      "A copy of your session's traffic is sent to your local process. The deployed service still handles every request, including yours.",
    recommended: false,
    target_types: ['deployment', 'statefulset', 'rollout', 'pod'],
    targetless: false,
    http_filter: true,
    traffic_question: 'Customize the header filter and ports?',
    traffic_default:
      'Default: `baggage: .*mirrord-session={key}.*` header filter on every port your app listens on.',
  },
  {
    id: 'replace',
    label: 'Replace',
    description:
      'Every request reaches your local process. The deployed workload is scaled to zero for the session and restored when it ends.',
    recommended: false,
    target_types: ['deployment', 'statefulset'],
    targetless: false,
    http_filter: false,
    traffic_question: 'Customize ports?',
    traffic_default:
      'Default: all traffic on every port your app listens on, unfiltered.',
  },
]

/** `GET /api/v1/up/workloads` by mode. */
export const WORKLOADS: Record<string, string[]> = {
  split: ['deployment/server-echo', 'pod/server-echo-abc/container/echo'],
  mirror: ['deployment/server-echo', 'pod/server-echo-abc/container/echo'],
  replace: ['deployment/server-echo'],
}

/** `GET /api/v1/up/ports` by target path. */
export const PORTS: Record<string, number[]> = {
  'deployment/server-echo': [80, 9090],
  'pod/server-echo-abc/container/echo': [80],
}

/** Bodies posted to `/api/v1/up/render`, oldest first. */
export const renderRequests: PickedService[][] = []

export const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })

/** Replaces `fetch`. `overrides` answer routes by path instead, e.g. with an error. */
export const installFakeServer = (
  overrides: Record<string, () => Response> = {},
) => {
  renderRequests.length = 0
  vi.stubGlobal(
    'fetch',
    vi.fn((input: string, init?: RequestInit) => {
      const path = new URL(input).pathname
      const override = overrides[path]
      if (override) return Promise.resolve(override())
      switch (path) {
        case '/api/v1/up/modes':
          return Promise.resolve(json({ modes: MODES }))
        case '/api/v2/kube/contexts':
          return Promise.resolve(
            json({
              contexts: [{ name: 'minikube', namespace: null }],
              current: 'minikube',
            }),
          )
        case '/api/v2/kube/namespaces':
          return Promise.resolve(json({ namespaces: ['default', 'staging'] }))
        case '/api/v1/up/workloads': {
          const mode = new URL(input).searchParams.get('mode') ?? ''
          return Promise.resolve(json({ workloads: WORKLOADS[mode] ?? [] }))
        }
        case '/api/v1/up/ports': {
          const target = new URL(input).searchParams.get('path') ?? ''
          return Promise.resolve(json({ ports: PORTS[target] ?? [] }))
        }
        case '/api/v1/up/render': {
          const { services } = JSON.parse(init?.body as string) as {
            services: PickedService[]
          }
          renderRequests.push(services)
          return Promise.resolve(
            json({ yaml: services.map((s) => `${s.name}:\n`).join('') }),
          )
        }
        default:
          return Promise.resolve(json({ error: 'not found' }, 404))
      }
    }),
  )
}
