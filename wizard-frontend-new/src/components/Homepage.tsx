import { useState } from "react";
import { ArrowRight, Sparkles, BookOpen } from "lucide-react";
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
    <div className="w-full max-w-lg mx-auto animate-fade-in">
      <Card className="shadow-xl border-[var(--border)] overflow-hidden relative">
        {/* Decorative gradient header */}
        <div className="h-32 bg-gradient-to-br from-primary via-primary/80 to-primary/60 relative overflow-hidden">
          <div className="absolute inset-0 bg-[radial-gradient(circle_at_30%_50%,rgba(255,255,255,0.12),transparent_60%)]" />
          <div className="absolute bottom-0 left-0 right-0 h-8 bg-gradient-to-t from-[var(--card)] to-transparent" />
          <div className="absolute inset-0 flex items-center justify-center">
            <div className="p-4 rounded-2xl bg-white/15 backdrop-blur-sm border border-white/20 shadow-lg">
              <img src={MirrordLogo} alt="mirrord" className="h-10 brightness-0 invert" />
            </div>
          </div>
        </div>

        <CardContent className="pt-6 pb-8 px-8">
          {/* Header */}
          <div className="text-center mb-8">
            <h1 className="text-2xl font-bold text-[var(--foreground)] mb-2 tracking-tight">
              Configuration Wizard
            </h1>
            <p className="text-sm text-[var(--muted-foreground)] leading-relaxed max-w-sm mx-auto">
              Generate a{" "}
              <code className="px-1.5 py-0.5 rounded-md bg-[var(--muted)] text-primary font-semibold text-xs">
                mirrord.json
              </code>{" "}
              config to connect your local environment to a Kubernetes cluster.
            </p>
          </div>

          {/* Main CTA */}
          <div className="space-y-3">
            <Button
              onClick={() => openWizard("config")}
              className="w-full h-12 text-base font-medium shadow-brand hover:shadow-brand-hover transition-all duration-200 group"
            >
              <Sparkles className="h-4 w-4 mr-2 group-hover:scale-110 transition-transform" />
              Get Started
              <ArrowRight className="h-4 w-4 ml-2 group-hover:translate-x-0.5 transition-transform" />
            </Button>

            <button
              onClick={() => openWizard("learn")}
              className="w-full text-center text-sm text-[var(--muted-foreground)] hover:text-primary transition-all flex items-center justify-center gap-2 py-3 rounded-xl hover:bg-primary/5 border border-transparent hover:border-primary/10"
            >
              <BookOpen className="h-4 w-4" />
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
