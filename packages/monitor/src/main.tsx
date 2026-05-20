import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './App'
import { autoConfigureExtension } from './extensionConfigure'
import { ErrorBoundary } from './components/ErrorBoundary'
import { emitUserBlocked } from './analytics'
import '@metalbear/ui/styles.css'
import './index.css'

window.addEventListener('error', (event: ErrorEvent) => {
  emitUserBlocked('unhandled_error', 'health', {
    error: event.message ?? 'unknown',
    source: 'error',
  })
})

window.addEventListener('unhandledrejection', (event: PromiseRejectionEvent) => {
  const reason = event.reason
  const error =
    reason instanceof Error
      ? reason.message
      : typeof reason === 'string'
        ? reason
        : 'unknown rejection'
  emitUserBlocked('unhandled_error', 'health', {
    error,
    source: 'unhandledrejection',
  })
})

autoConfigureExtension()

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <ErrorBoundary component="App">
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
)
