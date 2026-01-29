import { useState, useContext } from "react";
import { ChevronLeft, ChevronRight, X } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogFooter,
  DialogTitle,
  DialogDescription,
  DialogClose,
  Button,
  Badge,
} from "@metalbear/ui";
import { ConfigDataContext, DefaultConfig } from "./UserDataContext";
import { readBoilerplateType } from "./JsonUtils";

// Map internal mode names to display names
const modeDisplayNames: Record<string, string> = {
  steal: "Filtering",
  mirror: "Mirror",
  replace: "Replace",
  custom: "Custom",
};

const modeColors: Record<string, string> = {
  steal: "bg-secondary/20 text-secondary-foreground border-secondary/30",
  mirror: "bg-primary/10 text-primary border-primary/20",
  replace: "bg-destructive/10 text-destructive border-destructive/20",
  custom: "bg-[var(--muted)] text-[var(--muted-foreground)] border-[var(--border)]",
};

import BoilerplateStep from "./steps/BoilerplateStep";
import ConfigTabs from "./steps/ConfigTabs";
import LearningSteps from "./steps/LearningSteps";
import ErrorBoundary from "./ErrorBoundary";

interface WizardProps {
  open: boolean;
  onClose: () => void;
  startWithLearning?: boolean;
}

type WizardStep = "boilerplate" | "config" | "learning";

type ConfigTab = "target" | "network" | "export";

const Wizard = ({ open, onClose, startWithLearning = false }: WizardProps) => {
  const { config, setConfig } = useContext(ConfigDataContext)!;
  const [currentStep, setCurrentStep] = useState<WizardStep>(
    startWithLearning ? "learning" : "boilerplate"
  );
  const [learningComplete, setLearningComplete] = useState(false);
  const [currentTab, setCurrentTab] = useState<ConfigTab>("target");
  const [canAdvanceTab, setCanAdvanceTab] = useState(false);

  const handleClose = () => {
    setConfig(DefaultConfig);
    setCurrentStep(startWithLearning ? "learning" : "boilerplate");
    setLearningComplete(false);
    setCurrentTab("target");
    onClose();
  };

  const goToConfig = () => {
    setCurrentStep("boilerplate");
  };

  const goFromBoilerplate = () => {
    setCurrentStep("config");
    setCurrentTab("target");
  };

  const goBack = () => {
    if (currentStep === "config") {
      if (currentTab === "target") {
        setCurrentStep("boilerplate");
      } else if (currentTab === "network") {
        setCurrentTab("target");
      } else if (currentTab === "export") {
        setCurrentTab("network");
      }
    } else if (currentStep === "boilerplate" && learningComplete) {
      setCurrentStep("learning");
    }
  };

  const goNext = () => {
    if (currentStep === "config") {
      if (currentTab === "target") {
        setCurrentTab("network");
      } else if (currentTab === "network") {
        setCurrentTab("export");
      }
    }
  };

  const handleLearningComplete = () => {
    setLearningComplete(true);
    setCurrentStep("boilerplate");
  };

  const boilerplateType = readBoilerplateType(config);

  const getStepInfo = () => {
    switch (currentStep) {
      case "learning":
        return { title: "Learn mirrord", step: 1, total: 3 };
      case "boilerplate":
        return { title: "Choose Mode", step: learningComplete ? 2 : 1, total: learningComplete ? 3 : 2 };
      case "config":
        return { title: "Configure", step: learningComplete ? 3 : 2, total: learningComplete ? 3 : 2 };
      default:
        return { title: "", step: 0, total: 0 };
    }
  };

  const stepInfo = getStepInfo();

  return (
    <Dialog open={open} onOpenChange={(isOpen: boolean) => !isOpen && handleClose()}>
      <DialogContent className="max-w-2xl max-h-[90vh] overflow-y-auto bg-[var(--card)] border border-[var(--border)] shadow-xl [&>button]:hidden">
        <DialogHeader className="border-b border-[var(--border)] pb-4 -mx-6 px-6">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-3">
              <DialogTitle className="text-xl font-semibold text-[var(--foreground)]">
                {stepInfo.title}
              </DialogTitle>
              {currentStep !== "learning" && boilerplateType !== "custom" && (
                <Badge
                  variant="outline"
                  className={`capitalize text-xs font-medium ${modeColors[boilerplateType] || modeColors.custom}`}
                >
                  {modeDisplayNames[boilerplateType] || boilerplateType}
                </Badge>
              )}
            </div>
            <div className="flex items-center gap-4">
              {/* Step indicator */}
              <div className="flex items-center gap-2">
                {Array.from({ length: stepInfo.total }).map((_, index) => (
                  <div
                    key={index}
                    className={`h-2 rounded-full transition-all duration-300 ${
                      index + 1 === stepInfo.step
                        ? "w-6 bg-primary"
                        : index + 1 < stepInfo.step
                        ? "w-2 bg-primary/50"
                        : "w-2 bg-[var(--muted)]"
                    }`}
                  />
                ))}
              </div>
              <DialogClose asChild>
                <button className="rounded-full p-2 hover:bg-[var(--muted)] transition-colors">
                  <X className="h-4 w-4 text-[var(--muted-foreground)]" />
                </button>
              </DialogClose>
            </div>
          </div>
        </DialogHeader>
        <DialogDescription className="sr-only">
          mirrord configuration wizard
        </DialogDescription>

        <div className="py-6">
          {currentStep === "learning" && (
            <LearningSteps onComplete={handleLearningComplete} />
          )}
          {currentStep === "boilerplate" && (
            <BoilerplateStep />
          )}
          {currentStep === "config" && (
            <ErrorBoundary>
              <ConfigTabs
                currentTab={currentTab}
                onTabChange={setCurrentTab}
                onCanAdvanceChange={setCanAdvanceTab}
              />
            </ErrorBoundary>
          )}
        </div>

        <DialogFooter className="flex justify-between sm:justify-between border-t border-[var(--border)] pt-4 -mx-6 px-6">
          {currentStep !== "learning" && currentStep !== "config" && (
            <>
              <div>
                {(currentStep === "boilerplate" && learningComplete) && (
                  <Button variant="outline" onClick={goBack} className="gap-2">
                    <ChevronLeft className="h-4 w-4" />
                    Back
                  </Button>
                )}
              </div>
              <div>
                {currentStep === "boilerplate" && (
                  <Button onClick={goFromBoilerplate} className="gap-2 shadow-brand hover:shadow-brand-hover">
                    Continue
                    <ChevronRight className="h-4 w-4" />
                  </Button>
                )}
              </div>
            </>
          )}

          {currentStep === "learning" && (
            <>
              <div />
              <Button variant="outline" onClick={goToConfig} className="gap-2">
                Skip to Configuration
                <ChevronRight className="h-4 w-4" />
              </Button>
            </>
          )}

          {currentStep === "config" && (
            <>
              <Button variant="outline" onClick={goBack} className="gap-2">
                <ChevronLeft className="h-4 w-4" />
                Back
              </Button>
              {currentTab !== "export" && (
                <Button
                  onClick={goNext}
                  disabled={!canAdvanceTab}
                  className="gap-2 shadow-brand hover:shadow-brand-hover"
                >
                  Next
                  <ChevronRight className="h-4 w-4" />
                </Button>
              )}
              {currentTab === "export" && <div />}
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};

export default Wizard;
