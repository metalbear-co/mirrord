import { useEffect } from "react";
import { useNavigate } from "react-router-dom";
import Onboarding from "./Onboarding";

const Index = () => {
  const navigate = useNavigate();

  useEffect(() => {
    // Check if user has completed onboarding
    const hasCompletedOnboarding = localStorage.getItem('mirrord-onboarding-completed');
    const hasConfigs = JSON.parse(localStorage.getItem('mirrord-configs') || '[]').length > 0;
    
    if (hasCompletedOnboarding || hasConfigs) {
      // Redirect to dashboard for returning users
      navigate('/dashboard');
    }
  }, [navigate]);

  return <Onboarding />;
};

export default Index;
