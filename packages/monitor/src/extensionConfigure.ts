const EXTENSION_ID = 'bijejadnnfgjkfdocgocklekjhnhkhkf'

type ChromeRuntime = {
    sendMessage: (
        extensionId: string,
        message: unknown,
        callback?: (response: unknown) => void
    ) => void
    lastError?: { message?: string }
}

type ChromeGlobal = {
    runtime?: ChromeRuntime
}

// Mirrors the token resolution in `api.ts`: the token arrives as a `?token=` query param the
// first time `mirrord ui` hands the user its URL, and is then persisted to sessionStorage so
// reloads (and the extension's "Open mirrord ui" button, which links to the bare origin without
// a token) stay authenticated. Fall back to the stored token here too, otherwise an
// already-authenticated page opened without the query param would never sync the token back to
// the extension.
const TOKEN_STORAGE_KEY = 'mirrord_ui_token'

function resolveToken(): string | null {
    const urlToken = new URLSearchParams(window.location.search).get('token')
    if (urlToken) return urlToken
    try {
        return sessionStorage.getItem(TOKEN_STORAGE_KEY)
    } catch {
        return null
    }
}

export function autoConfigureExtension(): void {
    const token = resolveToken()
    if (!token) return

    const chromeGlobal = (window as unknown as { chrome?: ChromeGlobal }).chrome
    const sendMessage = chromeGlobal?.runtime?.sendMessage
    if (!sendMessage) return

    try {
        sendMessage.call(
            chromeGlobal!.runtime!,
            EXTENSION_ID,
            {
                type: 'mirrord-ui-configure',
                backend: window.location.origin,
                token,
            },
            () => {
                void chromeGlobal!.runtime!.lastError
            }
        )
    } catch {
        return
    }
}
