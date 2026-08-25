import App, { type MonitorProps } from './App'
import { autoConfigureExtension } from './extensionConfigure'
import { ErrorBoundary } from './components/ErrorBoundary'
import { emitUserBlocked } from './analytics'

// Root export for the session monitor, consumed by the `mirrord-ui` shell (see `packages/ui`).
// Everything that used to live in the standalone `main.tsx` — global error → analytics wiring, the
// extension auto-configure, and the analytics-instrumented error boundary — is set up here so it
// only runs when the monitor route is actually shown.

let bootstrapped = false

// A crash loop re-throws the same failure for as long as the tab stays open, so one stuck
// tab can emit orders of magnitude more blocked events than every healthy session put
// together. Report each distinct failure once per page load: the first occurrence is what
// identifies the bug, and the repeats only distort the ratio it feeds. The cap bounds the
// set against a page that produces an unbounded variety of failures.
const MAX_REPORTED_ERRORS = 50
const reportedErrors = new Set<string>()

// Unrelated bugs share a message often enough that the message alone is too coarse to
// identify one by: the throw site and the handler that saw it are part of its identity. A
// loop re-throwing from one site keeps all three stable, so it still reports once. A
// rejection carrying a non-Error has no stack to separate it from another of its kind,
// and those collapse together.
function unhandledErrorKey(
  error: string,
  source: string,
  thrown: unknown,
): string {
  const stack = thrown instanceof Error ? thrown.stack : undefined
  return [source, error, stack ?? ''].join('\u0000')
}

function reportUnhandledError(
  error: string,
  source: 'error' | 'unhandledrejection',
  thrown: unknown,
): void {
  const key = unhandledErrorKey(error, source, thrown)
  if (reportedErrors.has(key) || reportedErrors.size >= MAX_REPORTED_ERRORS) {
    return
  }
  reportedErrors.add(key)
  emitUserBlocked('unhandled_error', { error, source }, thrown)
}

function bootstrapOnce(): void {
  if (bootstrapped) {
    return
  }
  bootstrapped = true

  window.addEventListener('error', (event: ErrorEvent) => {
    reportUnhandledError(event.message, 'error', event.error)
  })

  window.addEventListener(
    'unhandledrejection',
    (event: PromiseRejectionEvent) => {
      const reason: unknown = event.reason
      const error =
        reason instanceof Error
          ? reason.message
          : typeof reason === 'string'
            ? reason
            : 'unknown rejection'
      reportUnhandledError(error, 'unhandledrejection', reason)
    },
  )

  void autoConfigureExtension()
}

export default function Monitor(props: MonitorProps) {
  bootstrapOnce()

  return (
    <ErrorBoundary component="App">
      <App {...props} />
    </ErrorBoundary>
  )
}
