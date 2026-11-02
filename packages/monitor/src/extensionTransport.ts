const EXTENSION_ID = 'bijejadnnfgjkfdocgocklekjhnhkhkf'
const PAGE_REQUEST_SOURCE = 'mirrord-ui'
const PAGE_RESPONSE_SOURCE = 'mirrord-extension'
const PAGE_RETRY_INTERVAL_MS = 100

interface ChromeRuntime {
  sendMessage: (
    extensionId: string,
    message: unknown,
    callback: (response: unknown) => void,
  ) => void
  lastError?: { message?: string }
}

export function isFirefox(): boolean {
  return navigator.userAgent.includes('Firefox/')
}

export function hasChromeRuntime(): boolean {
  return getChromeRuntime() !== null
}

function getChromeRuntime(): ChromeRuntime | null {
  const runtime = (
    window as unknown as { chrome?: { runtime?: ChromeRuntime } }
  ).chrome?.runtime
  if (!runtime || typeof runtime.sendMessage !== 'function') return null
  return runtime
}

function sendViaRuntime(
  runtime: ChromeRuntime,
  message: unknown,
  resolve: (response: unknown) => void,
): void {
  runtime.sendMessage(EXTENSION_ID, message, (response: unknown) => {
    resolve(runtime.lastError ? null : response)
  })
}

// Firefox has no `externally_connectable`; its build relays these via a localhost content script.
function sendViaPage(
  message: unknown,
  resolve: (response: unknown) => void,
): () => void {
  const id = crypto.randomUUID()
  const onMessage = (event: MessageEvent) => {
    const data = event.data as {
      source?: unknown
      id?: unknown
      response?: unknown
    } | null
    if (
      event.source !== window ||
      data?.source !== PAGE_RESPONSE_SOURCE ||
      data.id !== id
    ) {
      return
    }
    resolve(data.response ?? null)
  }
  window.addEventListener('message', onMessage)
  const post = () =>
    window.postMessage(
      { source: PAGE_REQUEST_SOURCE, id, message },
      window.location.origin,
    )
  post()
  // The relay content script loads asynchronously, so an early post can land before it listens.
  const retry = setInterval(post, PAGE_RETRY_INTERVAL_MS)
  return () => {
    clearInterval(retry)
    window.removeEventListener('message', onMessage)
  }
}

export function sendToExtension<T = unknown>(
  message: unknown,
  timeoutMs: number,
): Promise<T | null> {
  return new Promise((resolve) => {
    let settled = false
    let cleanup: (() => void) | undefined
    const settle = (response: unknown) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      cleanup?.()
      resolve(response as T | null)
    }
    const timer = setTimeout(() => settle(null), timeoutMs)
    try {
      const runtime = getChromeRuntime()
      if (runtime) {
        sendViaRuntime(runtime, message, settle)
      } else if (isFirefox()) {
        cleanup = sendViaPage(message, settle)
      } else {
        settle(null)
      }
    } catch {
      settle(null)
    }
  })
}
