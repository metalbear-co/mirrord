import posthog from 'posthog-js'

const POSTHOG_KEY = 'phc_wIZh92nyk4vu6HidiLFUzjW6piZlZszuWZZFBS7yHHe'
const POSTHOG_HOST = 'https://hog.metalbear.com'

let initialized = false

export function initAnalytics(
  telemetryEnabled: boolean,
  mirrordVersion?: string,
) {
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
  setMirrordVersion(mirrordVersion)
  posthog.capture('session_monitor_opened', { source: 'session-monitor' })
}

let registeredVersion: string | null = null

/**
 * Register the mirrord CLI version serving this UI as a super property, so every captured
 * event carries it. Users run whatever CLI version they installed and upgrade on their own
 * schedule, so without this a crash fixed and released weeks ago is indistinguishable from a
 * live regression: the alerts keep firing from old clients and there is no way to tell which
 * releases are still affected.
 */
export function setMirrordVersion(version: string | undefined) {
  if (!initialized || !version || registeredVersion === version) return
  registeredVersion = version
  posthog.register({ mirrord_version: version })
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

export type EventKind = 'user_action' | 'health'

export function emitUserBlocked(
  reason: string,
  kind: EventKind,
  properties: Record<string, unknown> = {},
): void {
  trackEvent('monitor_user_blocked', {
    reason,
    kind,
    surface: 'monitor',
    ...properties,
  })
}

export function emitUserSucceeded(
  reason: string,
  kind: EventKind,
  properties: Record<string, unknown> = {},
): void {
  trackEvent('monitor_user_succeeded', {
    reason,
    kind,
    surface: 'monitor',
    ...properties,
  })
}
