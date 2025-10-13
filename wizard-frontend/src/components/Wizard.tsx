import React, { useState, ReactNode, useContext } from "react";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { Badge } from "./ui/badge";
import { ConfigDataContext } from "./UserDataContext";

export interface WizardStep {
  id: string;
  title: string;
  content: ReactNode;
}

export interface WizardProps {
  steps: WizardStep[];
  onComplete?: () => void;
  className?: string;
  isOpen?: boolean;
  onClose?: () => void;
}

// Simplified WizardHeader for generic wizard
const WizardHeader = ({
  title,
  currentStep,
  totalSteps,
  fetchConfigBadge,
}: {
  title: string;
  currentStep: number;
  totalSteps: number;
  fetchConfigBadge: () => string;
}) => (
  <div className="bg-background p-4 flex-shrink-0">
    <div className="mb-4">
      <DialogTitle className="flex items-center gap-2">
        {title}
        {title === "Configuration Setup" && (
          <Badge variant="secondary">{fetchConfigBadge()}</Badge>
        )}
      </DialogTitle>
      <p className="text-sm text-muted-foreground">
        Step {currentStep + 1} of {totalSteps}
      </p>
    </div>
  </div>
);

// Simplified WizardFooter for generic wizard
const WizardFooter = ({
  onPrevious,
  onNext,
  isFirstStep,
  isLastStep,
}: {
  onPrevious: () => void;
  onNext: () => void;
  isFirstStep: boolean;
  isLastStep: boolean;
}) => (
  <div className="border-t bg-background p-4">
    <div className="flex items-center justify-between">
      <div className="flex items-center gap-2">
        <Button
          variant="outline"
          onClick={onPrevious}
          disabled={isFirstStep}
          className="flex items-center gap-2"
        >
          <ChevronLeft className="h-4 w-4" />
          Back
        </Button>
      </div>

      <div className="flex items-center gap-2">
        <Button onClick={onNext} className="flex items-center gap-2">
          {isLastStep ? "Complete" : "Next"}
          {!isLastStep && <ChevronRight className="h-4 w-4" />}
        </Button>
      </div>
    </div>
  </div>
);

export const Wizard: React.FC<WizardProps> = ({
  steps,
  onComplete,
  isOpen = true,
  onClose,
}) => {
  const [currentStep, setCurrentStep] = useState(0);

  const currentStepData = steps[currentStep];
  const isFirstStep = currentStep === 0;
  const isLastStep = currentStep === steps.length - 1;
  const config = useContext(ConfigDataContext);

  const fetchConfigBadge = () => {
    const mode = config.config.feature.network.incoming.mode; // todo: nullness :(
    if (mode === "mirror") {
      return "Mirror mode"
    }
    if (config.config.agent.scaledown && config.config.agent.copyTarget) {
      return "Replace mode";
    } else if (!config.config.agent.scaledown && !config.config.agent.copyTarget) {
      return "Filtering mode";
    }
    return "Custom mode";
  };

  const goToNext = () => {
    if (isLastStep) {
      onComplete?.();
      onClose?.();
    } else {
      const nextStep = currentStep + 1;
      setCurrentStep(nextStep);
    }
  };

  const goToPrevious = () => {
    if (!isFirstStep) {
      const prevStep = currentStep - 1;
      setCurrentStep(prevStep);
    }
  };

  if (!isOpen) return null;

  return (
    <Dialog open={isOpen} onOpenChange={onClose}>
      <DialogContent className="max-w-2xl max-h-[85vh] p-0 flex flex-col">
        {/* Fixed Header */}
        <WizardHeader
          title={currentStepData.title}
          currentStep={currentStep}
          totalSteps={steps.length}
          fetchConfigBadge={fetchConfigBadge}
        />

        {/* Scrollable Content */}
        <div className="flex-1 px-4 pt-2 pb-4 overflow-y-auto">
          <div className="min-h-[200px]">
            {React.cloneElement(currentStepData.content as React.ReactElement, {
              currentStep,
              totalSteps: steps.length,
            })}
          </div>
        </div>

        {/* Fixed Footer with Navigation */}
        <WizardFooter
          onPrevious={goToPrevious}
          onNext={goToNext}
          isFirstStep={isFirstStep}
          isLastStep={isLastStep}
        />
      </DialogContent>
    </Dialog>
  );
};

export default Wizard;
