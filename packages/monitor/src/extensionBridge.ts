import { emitUserBlocked } from './analytics'

declare const chrome: any | undefined

const EXTENSION_ID = 'bijejadnnfgjkfdocgocklekjhnhkhkf'
const PING_TIMEOUT_MS = 250
const REQUEST_TIMEOUT_MS = 1500

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

function send<T = unknown>(
  message: unknown,
  timeoutMs = REQUEST_TIMEOUT_MS
): Promise<T | null> {
  if (!hasChromeRuntime()) return Promise.resolve(null)
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

/**
 * Public name for the extension messaging transport. The extension is reachable directly via
 * Chrome's `externally_connectable`, so this is a thin wrapper over `send`.
 */
export function sendExtensionMessage<T = unknown>(
  message: unknown,
  timeoutMs = REQUEST_TIMEOUT_MS
): Promise<T | null> {
  return send<T>(message, timeoutMs)
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

// Cross-browser extension landing page (routes the visitor to the right store). UTM params
// attribute installs that originate from the local `mirrord ui` install prompt.
export const EXTENSION_INSTALL_URL =
  'https://metalbear.com/mirrord/extension' +
  '?utm_source=mirrord_ui&utm_medium=install_banner&utm_campaign=browser_extension'
