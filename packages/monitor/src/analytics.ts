import posthog from 'posthog-js'

const POSTHOG_KEY = 'phc_wIZh92nyk4vu6HidiLFUzjW6piZlZszuWZZFBS7yHHe'
const POSTHOG_HOST = 'https://hog.metalbear.com'

let initialized = false

export function initAnalytics(telemetryEnabled: boolean) {
  if (!telemetryEnabled || initialized) return
  posthog.init(POSTHOG_KEY, {
    api_host: POSTHOG_HOST,
    ui_host: 'https://us.posthog.com',
    person_profiles: 'identified_only',
    autocapture: false,
    capture_pageview: false,
  })
  initialized = true
  posthog.capture('session_monitor_opened', { source: 'session-monitor' })
}

export function trackEvent(event: string, properties?: Record<string, unknown>) {
  if (!initialized) return
  posthog.capture(event, { source: 'session-monitor', ...properties })
}
