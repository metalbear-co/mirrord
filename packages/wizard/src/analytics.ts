import type { QueryClient } from '@tanstack/react-query'
import {
  emitBlockedEvent,
  emitOpened,
  initAnalytics,
  readTelemetryPref,
  type EventKind,
} from '@mirrord/monitor/analytics'

const ERROR_MESSAGE_MAX_LEN = 300

/**
 * Wizard-side entry to the shared posthog instance. The monitor initializes analytics from its
 * session poll, but a `mirrord wizard` deep link never mounts the monitor — so the wizard has
 * to initialize on its own or every failure it hits stays invisible. There is no session
 * config to consult on this path; the stored user preference is the only gate.
 */
export function initWizardAnalytics(): void {
  initAnalytics(readTelemetryPref())
  emitOpened('wizard_opened', { source: 'wizard' })
}

export function emitWizardBlocked(
  reason: string,
  kind: EventKind,
  properties: Record<string, unknown> = {},
  error?: unknown,
): void {
  emitBlockedEvent(
    'wizard_user_blocked',
    'wizard',
    reason,
    kind,
    { source: 'wizard', ...properties },
    error,
  )
}

function errorMessage(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error)
  return message.slice(0, ERROR_MESSAGE_MAX_LEN)
}

/**
 * Report every query that settles into an error state, once per failure episode: a query that
 * keeps failing across refetches emits a single event until it succeeds again (the lesson from
 * the monitor's chaos-poll alert flood). The wizard is currently the only React Query consumer
 * in the merged UI, so subscribing to the shared cache is equivalent to subscribing to wizard
 * queries; if the monitor ever adopts React Query, scope this by query key.
 */
export function observeQueryFailures(queryClient: QueryClient): () => void {
  const failedQueries = new Set<string>()
  return queryClient.getQueryCache().subscribe((event) => {
    if (event.type !== 'updated') return
    if (event.action.type === 'success') {
      failedQueries.delete(event.query.queryHash)
      return
    }
    if (event.action.type !== 'error') return
    if (failedQueries.has(event.query.queryHash)) return
    failedQueries.add(event.query.queryHash)
    const [resourceKey] = event.query.queryKey as readonly unknown[]
    const resource = typeof resourceKey === 'string' ? resourceKey : 'unknown'
    emitWizardBlocked(
      `${resource}_load_failed`,
      'user_action',
      { error: errorMessage(event.action.error) },
      event.action.error,
    )
  })
}
