/**
 * Auto-configures the mirrord browser extension when the session monitor page
 * loads. Sends the page's origin + auth token to the extension via
 * `chrome.runtime.sendMessage`, which writes them into `chrome.storage.local`
 * so the popup picks them up without the user pasting a chrome-extension://
 * URL by hand.
 *
 * The extension's manifest declares `externally_connectable` for
 * `http://localhost/*` and `http://127.0.0.1/*`, which is why the CLI binds
 * the monitor on IPv4 loopback (an `[::1]` origin would not match).
 */

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

export function autoConfigureExtension(): void {
    const token = new URLSearchParams(window.location.search).get('token')
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
                // Best-effort: extension may not be installed. Read lastError so
                // Chrome doesn't log an unchecked-runtime-lastError warning.
                void chromeGlobal!.runtime!.lastError
            }
        )
    } catch {
        // Ignore — page still works, user can fall back to the manual configure URL.
    }
}
