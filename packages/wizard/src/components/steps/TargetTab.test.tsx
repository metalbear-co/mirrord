import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { render, screen } from '../../test/test-utils'
import TargetTab from './TargetTab'
import { strings } from '../../strings'

const jsonResponse = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })

const mockKubeRoutes = (contexts: () => Response) => {
  vi.mocked(fetch).mockImplementation((input) => {
    const url = String(input)
    if (url.includes('/api/v2/kube/contexts')) {
      return Promise.resolve(contexts())
    }
    if (url.includes('/api/v2/kube/namespaces')) {
      return Promise.resolve(jsonResponse({ namespaces: [] }))
    }
    if (url.includes('/api/v2/kube/target-types')) {
      return Promise.resolve(jsonResponse({ targetTypes: [] }))
    }
    return Promise.resolve(jsonResponse([]))
  })
}

describe('TargetTab', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', vi.fn())
  })

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('shows an actionable error when contexts cannot be loaded', async () => {
    mockKubeRoutes(() =>
      jsonResponse(
        { error: 'failed to read kubeconfig: no such file or directory' },
        500,
      ),
    )

    render(<TargetTab setTargetPorts={vi.fn()} />)

    expect(
      await screen.findByText(strings.targetTab.kubeContextsError, undefined, {
        timeout: 4000,
      }),
    ).toBeInTheDocument()
    expect(
      screen.getByText('failed to read kubeconfig: no such file or directory'),
    ).toBeInTheDocument()
    expect(
      screen.getByText(strings.targetTab.kubeContextsErrorHint),
    ).toBeInTheDocument()
  })

  it('falls back to the response status when the error body is not JSON', async () => {
    mockKubeRoutes(() => new Response('boom', { status: 500 }))

    render(<TargetTab setTargetPorts={vi.fn()} />)

    expect(
      await screen.findByText(strings.targetTab.kubeContextsError, undefined, {
        timeout: 4000,
      }),
    ).toBeInTheDocument()
    expect(
      screen.getByText('request failed with status 500'),
    ).toBeInTheDocument()
  })

  it('shows the context picker without an error when contexts load', async () => {
    mockKubeRoutes(() =>
      jsonResponse({
        contexts: [{ name: 'minikube', namespace: null }],
        current: 'minikube',
      }),
    )

    render(<TargetTab setTargetPorts={vi.fn()} />)

    expect(
      await screen.findByText(strings.targetTab.kubeContext),
    ).toBeInTheDocument()
    expect(
      screen.queryByText(strings.targetTab.kubeContextsError),
    ).not.toBeInTheDocument()
  })
})
