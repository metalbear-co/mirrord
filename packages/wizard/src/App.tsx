import { TooltipProvider } from '@metalbear/ui'
import { Toaster } from './components/Toaster'
import { ConfigDataContextProvider } from './components/UserDataContext'
import Homepage from './components/Homepage'

function App() {
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
