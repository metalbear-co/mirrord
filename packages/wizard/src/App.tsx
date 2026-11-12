import { TooltipProvider } from '@metalbear/ui'
import { Toaster } from './components/Toaster'
import { DraftContextProvider } from './components/DraftContext'
import Homepage from './components/Homepage'

function App() {
  return (
    <DraftContextProvider>
      <TooltipProvider>
        <div className="bg-background flex min-h-full items-center justify-center p-4">
          <Homepage />
        </div>
        <Toaster />
      </TooltipProvider>
    </DraftContextProvider>
  )
}

export default App
