import { useEffect } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { TooltipProvider } from '@metalbear/ui'
import { Toaster } from './components/Toaster'
import { ConfigDataContextProvider } from './components/UserDataContext'
import Homepage from './components/Homepage'
import { initWizardAnalytics, observeQueryFailures } from './analytics'

function App() {
  const queryClient = useQueryClient()

  useEffect(() => {
    initWizardAnalytics()
    return observeQueryFailures(queryClient)
  }, [queryClient])

  return (
    <ConfigDataContextProvider>
      <TooltipProvider>
        <div className="bg-background flex min-h-full items-center justify-center p-4">
          <Homepage />
        </div>
        <Toaster />
      </TooltipProvider>
    </ConfigDataContextProvider>
  )
}

export default App
