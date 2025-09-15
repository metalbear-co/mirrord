import React, { useState, ReactNode } from 'react';
import { ChevronLeft, ChevronRight } from 'lucide-react';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
} from '@/components/ui/dialog';
import { cn } from '@/lib/utils';

export interface WizardStep {
  id: string;
  title: string;
  content: ReactNode;
}

export interface WizardProps {
  steps: WizardStep[];
  onComplete?: (data: any) => void;
  className?: string;
  data?: any;
  onDataChange?: (data: any) => void;
  isOpen?: boolean;
  onClose?: () => void;
}

// Simplified WizardHeader for generic wizard
const WizardHeader = ({ title, currentStep, totalSteps }: { title: string; currentStep: number; totalSteps: number }) => (
  <div className="bg-background p-4 flex-shrink-0">
    <div className="mb-4">
      <h2 className="text-2xl font-bold">{title}</h2>
      <p className="text-sm text-muted-foreground">Step {currentStep + 1} of {totalSteps}</p>
    </div>
  </div>
);

// Simplified WizardFooter for generic wizard
const WizardFooter = ({ 
  onPrevious, 
  onNext, 
  isFirstStep, 
  isLastStep 
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
        <Button
          onClick={onNext}
          className="flex items-center gap-2"
        >
          {isLastStep ? 'Complete' : 'Next'}
          {!isLastStep && <ChevronRight className="h-4 w-4" />}
        </Button>
      </div>
    </div>
  </div>
);

export const Wizard: React.FC<WizardProps> = ({
  steps,
  onComplete,
  className,
  data = {},
  onDataChange,
  isOpen = true,
  onClose,
}) => {
  const [currentStep, setCurrentStep] = useState(0);
  const [wizardData, setWizardData] = useState(data);

  const currentStepData = steps[currentStep];
  const isFirstStep = currentStep === 0;
  const isLastStep = currentStep === steps.length - 1;

  const updateData = (newData: any) => {
    const updatedData = { ...wizardData, ...newData };
    setWizardData(updatedData);
    onDataChange?.(updatedData);
  };

  const goToNext = () => {
    if (isLastStep) {
      onComplete?.(wizardData);
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
        />

        {/* Scrollable Content */}
        <div className="flex-1 px-4 pt-2 pb-4 overflow-y-auto">
          <div className="min-h-[200px]">
            {React.cloneElement(currentStepData.content as React.ReactElement, {
              data: wizardData,
              updateData,
              currentStep,
              totalSteps: steps.length
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
