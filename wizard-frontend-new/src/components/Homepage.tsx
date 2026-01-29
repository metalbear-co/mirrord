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
    <div className="w-full max-w-md mx-auto animate-fade-in">
      <Card className="shadow-lg border-[var(--border)] overflow-hidden">
        {/* Decorative gradient header */}
        <div className="h-2 gradient-primary" />

        <CardContent className="pt-10 pb-8 px-8">
          {/* Logo */}
          <div className="flex justify-center mb-8">
            <div className="p-4 rounded-2xl bg-primary/5 border border-primary/10">
              <img
                src={MirrordLogo}
                alt="mirrord"
                className="h-12"
              />
            </div>
          </div>

          {/* Header */}
          <div className="text-center mb-10">
            <h1 className="text-2xl font-semibold text-[var(--foreground)] mb-3">
              Configuration Wizard
            </h1>
            <p className="text-sm text-[var(--muted-foreground)] leading-relaxed max-w-xs mx-auto">
              Generate a <code className="px-1.5 py-0.5 rounded bg-[var(--muted)] text-primary font-medium text-xs">mirrord.json</code> config file to connect your local environment to Kubernetes.
            </p>
          </div>

          {/* Main CTA */}
          <div className="space-y-4">
            <Button
              onClick={() => openWizard("config")}
              className="w-full h-12 text-base font-medium shadow-brand hover:shadow-brand-hover transition-all duration-200"
            >
              Get Started
              <ArrowRight className="h-4 w-4 ml-2" />
            </Button>

            <div className="relative">
              <div className="absolute inset-0 flex items-center">
                <div className="w-full border-t border-[var(--border)]" />
              </div>
              <div className="relative flex justify-center">
                <span className="bg-[var(--card)] px-3 text-xs text-[var(--muted-foreground)]">
                  or
                </span>
              </div>
            </div>

            <button
              onClick={() => openWizard("learn")}
              className="w-full text-center text-sm text-[var(--muted-foreground)] hover:text-primary transition-colors flex items-center justify-center gap-2 py-3 rounded-lg hover:bg-primary/5 group"
            >
              <Sparkles className="h-4 w-4 text-secondary group-hover:text-primary transition-colors" />
              <span>New to mirrord? Learn the basics first</span>
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
