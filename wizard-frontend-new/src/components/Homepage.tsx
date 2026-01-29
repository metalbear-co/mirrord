import { useState } from "react";
import { ArrowRight, Sparkles } from "lucide-react";
import { Button, Card, CardContent, MirrordLogo } from "@metalbear/ui";
import Wizard from "./Wizard";

type WizardFlow = "config" | "learn" | null;

const Homepage = () => {
  const [wizardOpen, setWizardOpen] = useState(false);
  const [wizardFlow, setWizardFlow] = useState<WizardFlow>(null);

  const openWizard = (flow: WizardFlow) => {
    setWizardFlow(flow);
    setWizardOpen(true);
  };

  const closeWizard = () => {
    setWizardOpen(false);
    setWizardFlow(null);
  };

  return (
    <div className="w-full max-w-md mx-auto">
      <Card className="shadow-lg">
        <CardContent className="pt-10 pb-8 px-8">
          {/* Logo */}
          <div className="flex justify-center mb-6">
            <img
              src={MirrordLogo}
              alt="mirrord"
              className="h-14"
            />
          </div>

          {/* Header */}
          <div className="text-center mb-8">
            <h1 className="text-2xl font-semibold text-[var(--foreground)] mb-3">
              Configuration Wizard
            </h1>
            <p className="text-sm text-[var(--muted-foreground)] leading-relaxed max-w-sm mx-auto">
              Generate a mirrord.json config file to connect your local environment to Kubernetes.
            </p>
          </div>

          {/* Main CTA */}
          <div className="space-y-4">
            <Button
              onClick={() => openWizard("config")}
              className="w-full h-11"
            >
              Get Started
              <ArrowRight className="h-4 w-4 ml-2" />
            </Button>

            <button
              onClick={() => openWizard("learn")}
              className="w-full text-center text-sm text-[var(--muted-foreground)] hover:text-primary transition-colors flex items-center justify-center gap-2 py-2"
            >
              <Sparkles className="h-4 w-4" />
              New to mirrord? Learn the basics first
            </button>
          </div>
        </CardContent>
      </Card>

      {/* Wizard Dialog */}
      <Wizard
        open={wizardOpen}
        onClose={closeWizard}
        startWithLearning={wizardFlow === "learn"}
      />
    </div>
  );
};

export default Homepage;
