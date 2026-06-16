import { emitUserBlocked } from './analytics'

declare const chrome: any | undefined

const EXTENSION_ID = 'bijejadnnfgjkfdocgocklekjhnhkhkf'
const PING_TIMEOUT_MS = 250
const REQUEST_TIMEOUT_MS = 1500

// Envelope types for the window.postMessage bridge. Firefox has no
// `externally_connectable`, so the extension injects a content script on
// localhost that relays these envelopes to/from its background. These strings
// must stay in sync with the extension (mirrord-browser:
// packages/core/src/constants.ts UI_BRIDGE_REQUEST_TYPE / UI_BRIDGE_RESPONSE_TYPE).
const UI_BRIDGE_REQUEST_TYPE = 'mirrord-ext-request'
const UI_BRIDGE_RESPONSE_TYPE = 'mirrord-ext-response'

export interface ExtensionState {
  installed: boolean
  supportsBridge: boolean
  version?: string
  joinedKey?: string | null
  hasBackend?: boolean
  watching?: boolean
}

const NOT_INSTALLED: ExtensionState = {
  installed: false,
  supportsBridge: false,
}

function hasChromeRuntime(): boolean {
  return (
    typeof chrome !== 'undefined' &&
    !!chrome.runtime &&
    typeof chrome.runtime.sendMessage === 'function'
  )
}

// Chrome path: message the extension directly via `externally_connectable`.
function chromeSend<T = unknown>(
  message: unknown,
  timeoutMs: number
): Promise<T | null> {
  return new Promise((resolve) => {
    let settled = false
    const timer = setTimeout(() => {
      if (settled) return
      settled = true
      resolve(null)
    }, timeoutMs)
    try {
      chrome.runtime.sendMessage(EXTENSION_ID, message, (response: unknown) => {
        if (settled) return
        settled = true
        clearTimeout(timer)
        if (chrome.runtime.lastError) {
          resolve(null)
          return
        }
        resolve(response as T)
      })
    } catch {
      if (settled) return
      settled = true
      clearTimeout(timer)
      resolve(null)
    }
  })
}

let bridgeSeq = 0

// Cross-browser path (Firefox): post a request the extension's localhost content
// script relays to its background, then await the matching response envelope.
function postMessageSend<T = unknown>(
  message: unknown,
  timeoutMs: number
): Promise<T | null> {
  if (typeof window === 'undefined') return Promise.resolve(null)
  return new Promise((resolve) => {
    const requestId = `mirrord-ui-${Date.now()}-${++bridgeSeq}`
    let settled = false
    const finish = (value: T | null) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      window.removeEventListener('message', onMessage)
      resolve(value)
    }
    const onMessage = (event: MessageEvent) => {
      if (event.source !== window) return
      const data = event.data as
        | { type?: string; requestId?: string; payload?: unknown }
        | null
      if (
        !data ||
        data.type !== UI_BRIDGE_RESPONSE_TYPE ||
        data.requestId !== requestId
      )
        return
      finish((data.payload ?? null) as T | null)
    }
    const timer = setTimeout(() => finish(null), timeoutMs)
    window.addEventListener('message', onMessage)
    window.postMessage(
      { type: UI_BRIDGE_REQUEST_TYPE, requestId, payload: message },
      window.location.origin
    )
  })
}

// Pick the transport: Chrome talks to the extension directly; everything else
// (Firefox) goes through the window.postMessage content-script bridge.
export function sendExtensionMessage<T = unknown>(
  message: unknown,
  timeoutMs = REQUEST_TIMEOUT_MS
): Promise<T | null> {
  return hasChromeRuntime()
    ? chromeSend<T>(message, timeoutMs)
    : postMessageSend<T>(message, timeoutMs)
}

function send<T = unknown>(
  message: unknown,
  timeoutMs = REQUEST_TIMEOUT_MS
): Promise<T | null> {
  return sendExtensionMessage<T>(message, timeoutMs)
}

export async function pingExtension(): Promise<ExtensionState> {
  const response = await send<{
    type?: string
    version?: string
    joinedKey?: string | null
    hasBackend?: boolean
    watching?: boolean
  }>({ type: 'ping' }, PING_TIMEOUT_MS)
  if (!response) return NOT_INSTALLED
  if (response.type !== 'pong') {
    return { installed: true, supportsBridge: false }
  }
  return {
    installed: true,
    supportsBridge: true,
    version: response.version,
    joinedKey: response.joinedKey ?? null,
    hasBackend: response.hasBackend,
    watching: response.watching,
  }
}

export async function joinViaExtension(
  key: string
): Promise<{ ok: boolean; joinedKey?: string | null; error?: string }> {
  const response = await send<{ type?: string; ok?: boolean; joinedKey?: string | null; error?: string }>({
    type: 'join',
    key,
  })
  if (!response) {
    emitUserBlocked('extension_bridge_failed', 'user_action', {
      action: 'join',
      error: 'No response from extension',
    })
    return { ok: false, error: 'No response from extension' }
  }
  if (response.type !== 'join_result') {
    emitUserBlocked('extension_bridge_failed', 'user_action', {
      action: 'join',
      error: 'Unsupported response',
    })
    return { ok: false, error: 'Unsupported response' }
  }
  return { ok: response.ok ?? false, joinedKey: response.joinedKey ?? null, error: response.error }
}

export async function leaveViaExtension(): Promise<{ ok: boolean; error?: string }> {
  const response = await send<{ type?: string; ok?: boolean; error?: string }>({ type: 'leave' })
  if (!response) {
    emitUserBlocked('extension_bridge_failed', 'user_action', {
      action: 'leave',
      error: 'No response from extension',
    })
    return { ok: false, error: 'No response from extension' }
  }
  if (response.type !== 'leave_result') {
    emitUserBlocked('extension_bridge_failed', 'user_action', {
      action: 'leave',
      error: 'Unsupported response',
    })
    return { ok: false, error: 'Unsupported response' }
  }
  return { ok: response.ok ?? false, error: response.error }
}

export const CHROME_WEB_STORE_URL =
  'https://chromewebstore.google.com/detail/mirrord/bijejadnnfgjkfdocgocklekjhnhkhkf'

// Cross-browser extension landing page (routes the visitor to the right store). UTM params
// attribute installs that originate from the local `mirrord ui` install banner.
export const EXTENSION_INSTALL_URL =
  'https://metalbear.com/mirrord/extension' +
  '?utm_source=mirrord_ui&utm_medium=install_banner&utm_campaign=browser_extension'
