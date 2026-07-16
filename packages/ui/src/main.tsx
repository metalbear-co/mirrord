import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { applyDark, loadTheme, resolveDark } from '@mirrord/monitor/theme'
import App from './App'
import { ErrorBoundary } from './ErrorBoundary'
import '@metalbear/ui/styles.css'
import './index.css'

// One shared theme for the whole app: apply the persisted light/dark preference before first paint.
// The monitor tab's header lets the user change it (persisted to the same key); the wizard tab
// inherits whatever was last chosen.
applyDark(resolveDark(loadTheme()))

// Shared across both tabs. The monitor doesn't use react-query today, but providing the client at
// the shell means the wizard (which does) works and future monitor use is free.
const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnWindowFocus: false,
      retry: 1,
      staleTime: 30000,
    },
  },
})

const rootElement = document.getElementById('root')
if (!rootElement) {
  throw new Error('Root element #root not found')
}

createRoot(rootElement).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <ErrorBoundary>
        <App />
      </ErrorBoundary>
    </QueryClientProvider>
  </StrictMode>,
)
