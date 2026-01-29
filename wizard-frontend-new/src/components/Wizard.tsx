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
  steal: "filtering",
  mirror: "mirror",
  replace: "replace",
  custom: "custom",
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

const Wizard = ({ open, onClose, startWithLearning = false }: WizardProps) => {
  const { config, setConfig } = useContext(ConfigDataContext)!;
  const [currentStep, setCurrentStep] = useState<WizardStep>(
    startWithLearning ? "learning" : "boilerplate"
  );
  const [learningComplete, setLearningComplete] = useState(false);

  const handleClose = () => {
    setConfig(DefaultConfig);
    setCurrentStep(startWithLearning ? "learning" : "boilerplate");
    setLearningComplete(false);
    onClose();
  };

  const goToConfig = () => {
    setCurrentStep("boilerplate");
  };

  const goFromBoilerplate = () => {
    setCurrentStep("config");
  };

  const goBack = () => {
    if (currentStep === "config") {
      setCurrentStep("boilerplate");
    } else if (currentStep === "boilerplate" && learningComplete) {
      setCurrentStep("learning");
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
      <DialogContent className="max-w-2xl max-h-[90vh] overflow-y-auto bg-[var(--card)] border border-[var(--border)] [&>button]:hidden">
        <DialogHeader>
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-3">
              <DialogTitle className="text-lg font-semibold">
                {stepInfo.title}
              </DialogTitle>
              {currentStep !== "learning" && boilerplateType !== "custom" && (
                <Badge variant="outline" className="capitalize">
                  {modeDisplayNames[boilerplateType] || boilerplateType} mode
                </Badge>
              )}
            </div>
            <div className="flex items-center gap-3">
              <span className="text-sm text-[var(--muted-foreground)]">
                Step {stepInfo.step} of {stepInfo.total}
              </span>
              <DialogClose asChild>
                <button className="rounded-full p-1.5 hover:bg-[var(--muted)] transition-colors">
                  <X className="h-4 w-4 text-[var(--muted-foreground)]" />
                </button>
              </DialogClose>
            </div>
          </div>
          <DialogDescription className="sr-only">
            mirrord configuration wizard
          </DialogDescription>
        </DialogHeader>

        <div className="py-4">
          {currentStep === "learning" && (
            <LearningSteps onComplete={handleLearningComplete} />
          )}
          {currentStep === "boilerplate" && (
            <BoilerplateStep />
          )}
          {currentStep === "config" && (
            <ErrorBoundary>
              <ConfigTabs />
            </ErrorBoundary>
          )}
        </div>

        <DialogFooter className="flex justify-between sm:justify-between">
          {currentStep !== "learning" && currentStep !== "config" && (
            <>
              <div>
                {(currentStep === "boilerplate" && learningComplete) && (
                  <Button variant="outline" onClick={goBack}>
                    <ChevronLeft className="h-4 w-4 mr-2" />
                    Back
                  </Button>
                )}
              </div>
              <div>
                {currentStep === "boilerplate" && (
                  <Button onClick={goFromBoilerplate}>
                    Continue
                    <ChevronRight className="h-4 w-4 ml-2" />
                  </Button>
                )}
              </div>
            </>
          )}

          {currentStep === "learning" && (
            <>
              <div />
              <Button variant="outline" onClick={goToConfig}>
                Skip to Configuration
                <ChevronRight className="h-4 w-4 ml-2" />
              </Button>
            </>
          )}

          {currentStep === "config" && <div className="w-full" />}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
};

export default Wizard;
