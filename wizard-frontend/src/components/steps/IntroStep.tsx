import { Server } from "lucide-react";
import type { WizardStep } from "../Wizard";

const IntroStep: () => WizardStep = () => {
  return {
    id: "intro-new-user",
    title: "Welcome to mirrord Configuration",
    content: (
      <div className="space-y-6">
        <div className="text-center space-y-4">
          <div className="mx-auto w-24 h-24 bg-gradient-to-br from-primary to-primary/60 rounded-full flex items-center justify-center">
            <Server className="h-12 w-12 text-white" />
          </div>

          <div className="space-y-2">
            <h3 className="text-lg font-semibold">Get started with mirrord</h3>
            <p className="text-muted-foreground max-w-md mx-auto">
              Run local code like it's in your Kubernetes cluster without
              deploying it first. Get started by creating your first
              mirrord.json configuration.
            </p>
          </div>
        </div>
      </div>
    ),
  };
};

export default IntroStep;
