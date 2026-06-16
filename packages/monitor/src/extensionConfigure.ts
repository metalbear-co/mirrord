import { sendExtensionMessage } from './extensionBridge'

const CONFIGURE_TYPE = 'mirrord-ui-configure'
const CONFIGURE_TIMEOUT_MS = 500
// On Firefox the configure message rides the content-script bridge, which may not
// have registered its listener yet when the page first loads. Retry a few times
// (the message is idempotent) until the extension acknowledges with `{ ok: true }`.
const CONFIGURE_ATTEMPTS = 5
const CONFIGURE_RETRY_MS = 400

/**
 * Dispatched on `window` once the extension acknowledges configuration. The React app
 * listens for it (and checks `isExtensionConfigured()` on mount, in case the ack landed
 * before it rendered) to show a confirmation banner.
 */
export const EXTENSION_CONFIGURED_EVENT = 'mirrord:extension-configured'

let extensionConfigured = false

/** Whether the extension has acknowledged configuration during this page load. */
export function isExtensionConfigured(): boolean {
  return extensionConfigured
}

function markConfigured(): void {
  if (extensionConfigured) return
  extensionConfigured = true
  window.dispatchEvent(new CustomEvent(EXTENSION_CONFIGURED_EVENT))
}

/**
 * Push the current `mirrord ui` backend + token to the browser extension so it can
 * talk to this poller without the user re-entering anything. Chrome receives it via
 * `externally_connectable`; Firefox via the localhost content-script bridge. No-op
 * when the extension isn't installed (the send simply times out).
 */
export function autoConfigureExtension(): void {
  const token = new URLSearchParams(window.location.search).get('token')
  if (!token) return

  const message = {
    type: CONFIGURE_TYPE,
    backend: window.location.origin,
    token,
  }

  void (async () => {
    for (let attempt = 0; attempt < CONFIGURE_ATTEMPTS; attempt++) {
      const response = await sendExtensionMessage<{ ok?: boolean }>(
        message,
        CONFIGURE_TIMEOUT_MS
      )
      if (response && response.ok) {
        markConfigured()
        return
      }
      await new Promise((r) => setTimeout(r, CONFIGURE_RETRY_MS))
    }
  })()
}
