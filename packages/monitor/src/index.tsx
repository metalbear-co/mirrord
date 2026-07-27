import App, { type MonitorProps } from './App'
import { autoConfigureExtension } from './extensionConfigure'
import { ErrorBoundary } from './components/ErrorBoundary'
import { emitUserBlocked } from './analytics'

// Root export for the session monitor, consumed by the `mirrord-ui` shell (see `packages/ui`).
// Everything that used to live in the standalone `main.tsx` — global error → analytics wiring, the
// extension auto-configure, and the analytics-instrumented error boundary — is set up here so it
// only runs when the monitor route is actually shown.

let bootstrapped = false

function bootstrapOnce(): void {
  if (bootstrapped) {
    return
  }
  bootstrapped = true

  window.addEventListener('error', (event: ErrorEvent) => {
    emitUserBlocked(
      'unhandled_error',
      'health',
      {
        error: event.message,
        source: 'error',
      },
      event.error,
    )
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
      emitUserBlocked(
        'unhandled_error',
        'health',
        {
          error,
          source: 'unhandledrejection',
        },
        reason,
      )
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
