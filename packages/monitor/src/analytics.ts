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
  // The PostHog project's remote config gates session replay on URL triggers matching
  // public marketing/app domains — which never match the local monitor UI. Force-start the
  // recorder here so we do capture replays from the UI; the masking above keeps the content
  // safe regardless of where the recorder runs. Overriding every gate (sampling,
  // linked_flag, trigger) makes this robust to future project-config changes too.
  posthog.startSessionRecording({ sampling: true, linked_flag: true, url_trigger: true })
}

/**
 * Runtime toggle for the user telemetry preference. If init has already happened, this
 * flips posthog's opt-in state and starts or stops the session recorder. If init has not
 * run yet (no active sessions, or the user opened with telemetry off), this is a no-op —
 * the `telemetryEnabled` argument passed to `initAnalytics` later will be authoritative.
 */
export function setTelemetryEnabled(enabled: boolean) {
  if (!initialized) return
  if (enabled) {
    posthog.opt_in_capturing()
    posthog.startSessionRecording()
  } else {
    posthog.stopSessionRecording()
    posthog.opt_out_capturing()
  }
}

export function trackEvent(event: string, properties?: Record<string, unknown>) {
  if (!initialized) return
  posthog.capture(event, { source: 'session-monitor', ...properties })
}
