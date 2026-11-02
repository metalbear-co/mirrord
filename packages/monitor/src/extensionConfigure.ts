import {
  hasChromeRuntime,
  isFirefox,
  sendToExtension,
} from './extensionTransport'

const CONFIGURE_TIMEOUT_MS = 1500
const FIREFOX_CONFIGURE_TIMEOUT_MS = 5000

// The token arrives as a `?token=` query param the first time `mirrord ui` hands the user its
// URL. When the page is opened without it — the extension's "Open mirrord ui" button links to the
// bare origin, and reloads drop the query param too — the page still authenticates via the
// `mirrord_token` cookie, but that cookie is `HttpOnly` so JS can't read it, and `sessionStorage`
// is per-tab so a freshly opened tab has nothing either. In that case ask the backend (which the
// browser authenticates with the cookie automatically) for the token so we can still forward it to
// the extension.
async function resolveToken(): Promise<string | null> {
  const urlToken = new URLSearchParams(window.location.search).get('token')
  if (urlToken) return urlToken
  try {
    const resp = await fetch('/api/token', { credentials: 'same-origin' })
    if (!resp.ok) return null
    const data = (await resp.json()) as { token?: string }
    return data.token ?? null
  } catch {
    return null
  }
}

export async function autoConfigureExtension(): Promise<void> {
  if (hasChromeRuntime()) {
    const token = await resolveToken()
    if (!token) return
    await sendToExtension(
      {
        type: 'mirrord-ui-configure',
        backend: window.location.origin,
        token,
      },
      CONFIGURE_TIMEOUT_MS,
    )
    return
  }
  if (!isFirefox()) return
  // The Firefox content script reads the token from this origin itself, so it never goes
  // through page messages that other scripts on the page could observe.
  await sendToExtension(
    { type: 'mirrord-ui-configure' },
    FIREFOX_CONFIGURE_TIMEOUT_MS,
  )
}
