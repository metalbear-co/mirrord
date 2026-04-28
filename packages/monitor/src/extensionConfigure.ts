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
                void chromeGlobal!.runtime!.lastError
            }
        )
    } catch {
        return
    }
}
