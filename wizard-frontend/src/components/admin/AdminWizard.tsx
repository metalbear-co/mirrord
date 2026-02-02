import { useState, type ReactNode } from "react";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "../ui/dialog";
import { Button } from "../ui/button";

export interface AdminWizardStep {
  id: string;
  title: string;
  content: ReactNode;
}

interface AdminWizardProps {
  steps: AdminWizardStep[];
  isOpen: boolean;
  onClose: () => void;
}

const AdminWizard = ({ steps, isOpen, onClose }: AdminWizardProps) => {
  const [currentStep, setCurrentStep] = useState(0);

  const currentStepData = steps[currentStep];
  const isFirstStep = currentStep === 0;
  const isLastStep = currentStep === steps.length - 1;

  const goToNext = () => {
    if (isLastStep) {
      onClose();
    } else {
      setCurrentStep((prev) => prev + 1);
    }
  };

  const goToPrevious = () => {
    if (!isFirstStep) {
      setCurrentStep((prev) => prev - 1);
    }
  };

  const closeWizard = () => {
    setCurrentStep(0);
    onClose();
  };

  if (!isOpen) return null;

  return (
    <Dialog open={isOpen} onOpenChange={closeWizard}>
      <DialogContent className="max-w-3xl max-h-[85vh] p-0 flex flex-col">
        <DialogDescription className="sr-only">
          Admin dashboard wizard dialog
        </DialogDescription>
        <div className="bg-background p-4 flex-shrink-0">
          <div className="mb-4">
            <DialogTitle className="flex items-center gap-2">
              {currentStepData?.title ?? ""}
            </DialogTitle>
            <p className="text-sm text-muted-foreground">
              Step {currentStep + 1} of {steps.length}
            </p>
          </div>
        </div>

        <div className="flex-1 px-4 pt-2 pb-4 overflow-y-auto">
          <div className="min-h-[200px]">{currentStepData?.content}</div>
        </div>

        <div className="border-t bg-background p-4">
          <div className="flex items-center justify-between">
            <Button
              variant="outline"
              onClick={goToPrevious}
              disabled={isFirstStep}
              className="flex items-center gap-2"
            >
              <ChevronLeft className="h-4 w-4" />
              Back
            </Button>
            <Button onClick={goToNext} className="flex items-center gap-2">
              {isLastStep ? "Close" : "Next"}
              {!isLastStep && <ChevronRight className="h-4 w-4" />}
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
};

export default AdminWizard;
