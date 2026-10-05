import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiError, api, requestContext } from './api'

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('api request failures', () => {
  it('names the endpoint when fetch never reaches a response', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockRejectedValue(new TypeError('Failed to fetch')),
    )

    const err = await api.listChaosRules('abc').catch((e: unknown) => e)

    expect(err).toBeInstanceOf(ApiError)
    expect(requestContext(err)).toEqual({ endpoint: 'chaos_rules' })
  })

  it('reports the endpoint and status when the server answers with an error', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response('rules unavailable', {
          status: 503,
          statusText: 'Service Unavailable',
        }),
      ),
    )

    const err = await api.listChaosRules('abc').catch((e: unknown) => e)

    expect(requestContext(err)).toEqual({
      endpoint: 'chaos_rules',
      status: 503,
    })
    expect((err as ApiError).message).toBe('rules unavailable')
  })

  it('contributes nothing for an error that did not come from a request', () => {
    expect(requestContext(new Error('boom'))).toEqual({})
  })
})
