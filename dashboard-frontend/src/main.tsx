import React from 'react'
import ReactDOM from 'react-dom/client'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import App from './App'
import { applyTheme } from './lib/theme'
import './index.css'

// Apply theme immediately on load
const savedDarkMode = localStorage.getItem('darkMode')
const isDarkMode = savedDarkMode ? JSON.parse(savedDarkMode) : false
applyTheme(isDarkMode)

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnWindowFocus: false,
      retry: 1,
      staleTime: 30000,
    },
  },
})

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>
  </React.StrictMode>,
)
