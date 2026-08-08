import { beforeEach, describe, expect, it, vi } from 'vitest'
import { QueryClient } from '@tanstack/react-query'
import {
  initWizardAnalytics,
  emitWizardBlocked,
  observeQueryFailures,
} from './analytics'
import {
  emitBlockedEvent,
  emitOpened,
  initAnalytics,
  readTelemetryPref,
} from '@mirrord/monitor/analytics'

vi.mock('@mirrord/monitor/analytics', () => ({
  emitBlockedEvent: vi.fn(),
  emitOpened: vi.fn(),
  initAnalytics: vi.fn(),
  readTelemetryPref: vi.fn(() => true),
}))

const failWith = (message: string) => () =>
  Promise.reject(new Error(message)) as Promise<string>

async function fetchIgnoringError(
  queryClient: QueryClient,
  queryKey: string[],
  queryFn: () => Promise<string>,
) {
  await queryClient
    .fetchQuery({ queryKey, queryFn, retry: false, staleTime: 0 })
    .catch(() => undefined)
}

describe('initWizardAnalytics', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('initializes analytics with the stored preference and marks the wizard opened', () => {
    vi.mocked(readTelemetryPref).mockReturnValue(true)
    initWizardAnalytics()
    expect(initAnalytics).toHaveBeenCalledWith(true)
    expect(emitOpened).toHaveBeenCalledWith('wizard_opened', {
      source: 'wizard',
    })
  })

  it('passes an opt-out through to init', () => {
    vi.mocked(readTelemetryPref).mockReturnValue(false)
    initWizardAnalytics()
    expect(initAnalytics).toHaveBeenCalledWith(false)
  })
})

describe('emitWizardBlocked', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('emits the wizard surface event with the wizard source', () => {
    const error = new Error('boom')
    emitWizardBlocked('ui_crashed', 'user_action', { extra: 1 }, error)
    expect(emitBlockedEvent).toHaveBeenCalledWith(
      'wizard_user_blocked',
      'wizard',
      'ui_crashed',
      'user_action',
      { source: 'wizard', extra: 1 },
      error,
    )
  })
})

describe('observeQueryFailures', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('reports a failed query once per failure episode', async () => {
    const queryClient = new QueryClient()
    const unsubscribe = observeQueryFailures(queryClient)

    await fetchIgnoringError(queryClient, ['kubeContexts'], failWith('down'))
    await fetchIgnoringError(
      queryClient,
      ['kubeContexts'],
      failWith('still down'),
    )

    expect(emitBlockedEvent).toHaveBeenCalledTimes(1)
    expect(emitBlockedEvent).toHaveBeenCalledWith(
      'wizard_user_blocked',
      'wizard',
      'kubeContexts_load_failed',
      'user_action',
      { source: 'wizard', error: 'down' },
      expect.any(Error),
    )

    unsubscribe()
    queryClient.clear()
  })

  it('reports again after the query recovers and fails anew', async () => {
    const queryClient = new QueryClient()
    const unsubscribe = observeQueryFailures(queryClient)

    await fetchIgnoringError(queryClient, ['kubeNamespaces'], failWith('down'))
    await fetchIgnoringError(queryClient, ['kubeNamespaces'], () =>
      Promise.resolve('ok'),
    )
    await fetchIgnoringError(
      queryClient,
      ['kubeNamespaces'],
      failWith('down again'),
    )

    expect(emitBlockedEvent).toHaveBeenCalledTimes(2)

    unsubscribe()
    queryClient.clear()
  })

  it('reports distinct queries independently', async () => {
    const queryClient = new QueryClient()
    const unsubscribe = observeQueryFailures(queryClient)

    await fetchIgnoringError(queryClient, ['kubeContexts'], failWith('a'))
    await fetchIgnoringError(
      queryClient,
      ['targetDetails', 'ctx', 'ns'],
      failWith('b'),
    )

    expect(emitBlockedEvent).toHaveBeenCalledTimes(2)
    expect(emitBlockedEvent).toHaveBeenLastCalledWith(
      'wizard_user_blocked',
      'wizard',
      'targetDetails_load_failed',
      'user_action',
      { source: 'wizard', error: 'b' },
      expect.any(Error),
    )

    unsubscribe()
    queryClient.clear()
  })
})
