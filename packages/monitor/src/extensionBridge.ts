import { isFirefox, sendToExtension } from './extensionTransport'
import { emitUserBlocked } from './analytics'

const PING_TIMEOUT_MS = 250
const REQUEST_TIMEOUT_MS = 1500

export interface ExtensionState {
  installed: boolean
  supportsBridge: boolean
  version?: string | undefined
  joinedKey?: string | null
  hasBackend?: boolean | undefined
  watching?: boolean | undefined
}

const NOT_INSTALLED: ExtensionState = {
  installed: false,
  supportsBridge: false,
}

function send<T = unknown>(
  message: unknown,
  timeoutMs = REQUEST_TIMEOUT_MS,
): Promise<T | null> {
  return sendToExtension<T>(message, timeoutMs)
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
  key: string,
  context: string | null,
): Promise<{
  ok: boolean
  joinedKey?: string | null
  error?: string | undefined
}> {
  const response = await send<{
    type?: string
    ok?: boolean
    joinedKey?: string | null
    error?: string
  }>({
    type: 'join',
    key,
    ...(context && { context }),
  })
  if (!response) {
    emitUserBlocked('extension_bridge_failed', {
      action: 'join',
      error: 'No response from extension',
    })
    return { ok: false, error: 'No response from extension' }
  }
  if (response.type !== 'join_result') {
    emitUserBlocked('extension_bridge_failed', {
      action: 'join',
      error: 'Unsupported response',
    })
    return { ok: false, error: 'Unsupported response' }
  }
  return {
    ok: response.ok ?? false,
    joinedKey: response.joinedKey ?? null,
    error: response.error,
  }
}

export async function leaveViaExtension(): Promise<{
  ok: boolean
  error?: string | undefined
}> {
  const response = await send<{ type?: string; ok?: boolean; error?: string }>({
    type: 'leave',
  })
  if (!response) {
    emitUserBlocked('extension_bridge_failed', {
      action: 'leave',
      error: 'No response from extension',
    })
    return { ok: false, error: 'No response from extension' }
  }
  if (response.type !== 'leave_result') {
    emitUserBlocked('extension_bridge_failed', {
      action: 'leave',
      error: 'Unsupported response',
    })
    return { ok: false, error: 'Unsupported response' }
  }
  return { ok: response.ok ?? false, error: response.error }
}

const CHROME_WEB_STORE_URL =
  'https://chromewebstore.google.com/detail/mirrord/bijejadnnfgjkfdocgocklekjhnhkhkf'

const FIREFOX_ADDONS_URL = 'https://addons.mozilla.org/firefox/addon/mirrord/'

export const EXTENSION_INSTALL_URL = isFirefox()
  ? FIREFOX_ADDONS_URL
  : CHROME_WEB_STORE_URL
