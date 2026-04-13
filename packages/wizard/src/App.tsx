import { TooltipProvider } from "@metalbear/ui";
import { Toaster } from "./components/Toaster";
import { ConfigDataContextProvider } from "./components/UserDataContext";
import Homepage from "./components/Homepage";

function App() {
  return (
    <ConfigDataContextProvider>
      <TooltipProvider>
        <div className="min-h-screen bg-[var(--background)] flex items-center justify-center p-4">
          <Homepage />
        </div>
        <Toaster />
      </TooltipProvider>
    </ConfigDataContextProvider>
  );
}

export default App;
