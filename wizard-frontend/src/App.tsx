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
import AdminDashboard from "./pages/AdminDashboard";
import { UserDataContext } from "./components/UserDataContext";
import ALL_API_ROUTES from "./lib/routes";

const queryClient = new QueryClient();

const InnerApp = () => {
  const { error, isLoading, data } = useQuery({
    queryKey: ["userIsReturning"],
    queryFn: () =>
      fetch(window.location.origin + ALL_API_ROUTES.isReturning).then(async (res) =>
        res.ok && (await res.text()) === "true" ? true : false,
      ),
  });

  if (error) {
    console.error(error.message);
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
            <Route path="/admin-dashboard" element={<AdminDashboard />} />
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
