import { Toaster } from "./components/ui/toaster";
import { Toaster as Sonner } from "./components/ui/sonner";
import { TooltipProvider } from "./components/ui/tooltip";
import {
  QueryClient,
  QueryClientProvider,
  useQuery,
} from "@tanstack/react-query";
import { BrowserRouter, Routes, Route } from "react-router-dom";
import Homepage from "./pages/Homepage";
import NotFound from "./pages/NotFound";
import { UserDataContext } from "./components/UserDataContext";

const queryClient = new QueryClient();

const InnerApp = () => {
  const { error, isLoading, data } = useQuery({
    queryKey: ["userIsReturning"],
    queryFn: () =>
      fetch(window.location.href + "api/v1/is-returning").then(async (res) =>
        res.ok && (await res.text()) === "true" ? true : false,
      ),
  });

  if (error) {
    console.log(error);
  }

  return isLoading ? (
    <p>Loading...</p>
  ) : (
    <UserDataContext.Provider value={data}>
      <TooltipProvider>
        <Toaster />
        <Sonner />
        <BrowserRouter>
          <Routes>
            <Route path="/" element={<Homepage />} />
            {/* ADD ALL CUSTOM ROUTES ABOVE THE CATCH-ALL "*" ROUTE */}
            <Route path="*" element={<NotFound />} />
          </Routes>
        </BrowserRouter>
      </TooltipProvider>
    </UserDataContext.Provider>
  );
};

const App = () => {
  return (
    <QueryClientProvider client={queryClient}>
      <InnerApp />
    </QueryClientProvider>
  );
};

export default App;
