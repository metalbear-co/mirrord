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
    // The session monitor UI renders file paths, pod names, DNS hostnames, HTTP URLs, and
    // request/response bodies — all of which can contain customer data. Mask every visible
    // text node and every input value in session replays; behavioral data (clicks, nav,
    // scroll) is still useful without the raw content.
    session_recording: {
      maskAllInputs: true,
      maskTextSelector: '*',
    },
  })
  initialized = true
  posthog.capture('session_monitor_opened', { source: 'session-monitor' })
}

export function trackEvent(event: string, properties?: Record<string, unknown>) {
  if (!initialized) return
  posthog.capture(event, { source: 'session-monitor', ...properties })
}
