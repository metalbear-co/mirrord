import posthog from 'posthog-js'

const POSTHOG_KEY = 'phc_wIZh92nyk4vu6HidiLFUzjW6piZlZszuWZZFBS7yHHe'
const POSTHOG_HOST = 'https://hog.metalbear.com'

let initialized = false

/**
 * The telemetry state currently applied to posthog. Starts `false` to match an uninitialized
 * client, which captures nothing. Tracked because `posthog.opt_in_capturing()` is not
 * idempotent: every call captures a `$opt_in` event with `send_instantly`, whether or not the
 * client was already opted in. Callers re-assert the preference on a timer, so re-applying an
 * unchanged value would emit one event per tick.
 */
let appliedTelemetry = false

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
    // The PostHog project's remote config gates session replay on URL triggers matching
    // public marketing/app domains — which never match the local monitor UI. Force-start
    // the recorder in the `loaded` callback (after the recorder bundle is actually
    // available) so we do capture replays from the UI; the masking above keeps the content
    // safe regardless of where the recorder runs. Overriding every gate (sampling,
    // linked_flag, url_trigger) makes this robust to future project-config changes too.
    loaded: (ph) => {
      ph.startSessionRecording({
        sampling: true,
        linked_flag: true,
        url_trigger: true,
      })
    },
  })
  initialized = true
  // `posthog.init` leaves the client opted in, and init only runs with telemetry enabled, so
  // capturing is already in the desired state before any `setTelemetryEnabled` call arrives.
  appliedTelemetry = true
  posthog.capture('session_monitor_opened', { source: 'session-monitor' })
}

/**
 * Runtime toggle for the user telemetry preference. If init has already happened, this
 * flips posthog's opt-in state and starts or stops the session recorder. If init has not
 * run yet (no active sessions, or the user opened with telemetry off), this is a no-op —
 * the `telemetryEnabled` argument passed to `initAnalytics` later will be authoritative.
 *
 * Safe to call on every render or poll tick: only an actual change in the preference reaches
 * posthog.
 */
export function setTelemetryEnabled(enabled: boolean) {
  if (!initialized || enabled === appliedTelemetry) return
  appliedTelemetry = enabled
  if (enabled) {
    posthog.opt_in_capturing()
    posthog.startSessionRecording()
  } else {
    posthog.stopSessionRecording()
    posthog.opt_out_capturing()
  }
}

let licenseGroup: string | null = null

/**
 * Associate captured events with the operator's license group, keyed by the same license
 * fingerprint the operator reports in its own telemetry. This is what lets the dashboard
 * break session-monitor usage down by customer; without it these events are anonymous.
 * Only the operator knows the customer, so this is a no-op for OSS / non-operator users.
 */
export function setLicenseGroup(fingerprint: string, organization?: string) {
  if (!initialized || !fingerprint || licenseGroup === fingerprint) return
  licenseGroup = fingerprint
  posthog.group(
    'license',
    fingerprint,
    organization ? { name: organization } : undefined,
  )
}

export function trackEvent(
  event: string,
  properties?: Record<string, unknown>,
) {
  if (!initialized) return
  posthog.capture(event, { source: 'session-monitor', ...properties })
}

// Every event these two emit describes something a person was trying to do, including a
// crash, which stops them mid-task just as surely as a failed request does. Background
// liveness signals are deliberately not reported through here: they track how long a tab
// stayed open rather than whether anyone was affected, so mixing them in makes the
// blocked-versus-succeeded ratio unreadable. `reason` is the only axis a breakdown needs.
export function emitUserBlocked(
  reason: string,
  properties: Record<string, unknown> = {},
  error?: unknown,
): void {
  trackEvent('monitor_user_blocked', {
    reason,
    surface: 'monitor',
    ...properties,
  })
  if (error !== undefined && initialized) {
    posthog.captureException(error, {
      reason,
      surface: 'monitor',
      ...properties,
    })
  }
}

export function emitUserSucceeded(
  reason: string,
  properties: Record<string, unknown> = {},
): void {
  trackEvent('monitor_user_succeeded', {
    reason,
    surface: 'monitor',
    ...properties,
  })
}
