import { useState, useEffect } from 'react'
import { ChevronLeft, ChevronRight, Plus, X } from 'lucide-react'
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
} from '@metalbear/ui'
import { useDraft } from './DraftContext'
import { useModes } from '../lib/queries'
import {
  commandError,
  newDraft,
  targetDone,
  toPickedService,
  trafficError,
} from '../lib/up'
import { strings } from '../strings'
import ModeStep from './steps/ModeStep'
import TargetStep from './steps/TargetStep'
import TrafficStep from './steps/TrafficStep'
import RunStep from './steps/RunStep'
import ExportStep from './steps/ExportStep'
import LearningSteps from './steps/LearningSteps'
import ErrorBoundary from './ErrorBoundary'

interface WizardProps {
  open: boolean
  onClose: () => void
  startWithLearning?: boolean
}

/**
 * Each service goes through `mode`, `target`, `traffic` (only with a target) and `run`, like the
 * `mirrord up init` questions, then lands on `export`, which can start another.
 */
type WizardStep = 'learning' | 'mode' | 'target' | 'traffic' | 'run' | 'export'

const CONFIG_STEPS: WizardStep[] = [
  'mode',
  'target',
  'traffic',
  'run',
  'export',
]

const Wizard = ({ open, onClose, startWithLearning = false }: WizardProps) => {
  const { draft, setDraft, services, setServices, reset } = useDraft()
  const modes = useModes()
  const firstStep: WizardStep = startWithLearning ? 'learning' : 'mode'
  const [currentStep, setCurrentStep] = useState<WizardStep>(firstStep)
  const [learningComplete, setLearningComplete] = useState(false)
  const takenNames = services.map((service) => service.name)

  // Reset to the first step when the wizard opens
  useEffect(() => {
    if (open) {
      setCurrentStep(startWithLearning ? 'learning' : 'mode')
      setLearningComplete(false)
    }
  }, [open, startWithLearning])

  const handleClose = () => {
    reset()
    setCurrentStep(firstStep)
    setLearningComplete(false)
    onClose()
  }

  const goBack = () => {
    switch (currentStep) {
      case 'mode':
        // Back from a new service drops it.
        if (services.length > 0) {
          setDraft(newDraft())
          setCurrentStep('export')
        } else if (learningComplete) {
          setCurrentStep('learning')
        }
        break
      case 'target':
        setCurrentStep('mode')
        break
      case 'traffic':
        setCurrentStep('target')
        break
      case 'run':
        setCurrentStep(draft.targetless ? 'target' : 'traffic')
        break
      case 'learning':
      case 'export':
        break
    }
  }

  const goNext = () => {
    switch (currentStep) {
      case 'mode':
        setCurrentStep('target')
        break
      case 'target':
        setCurrentStep(draft.targetless ? 'run' : 'traffic')
        break
      case 'traffic':
        setCurrentStep('run')
        break
      case 'run':
        setServices([...services, toPickedService(draft)])
        setDraft(newDraft())
        setCurrentStep('export')
        break
      case 'learning':
      case 'export':
        break
    }
  }

  const canGoNext = (() => {
    switch (currentStep) {
      case 'mode':
        return modes.isSuccess
      case 'target':
        return targetDone(draft, takenNames)
      case 'traffic':
        return trafficError(draft) === null
      case 'run':
        return commandError(draft.command) === null
      case 'learning':
      case 'export':
        return false
    }
  })()

  const canGoBack =
    (currentStep === 'mode' && (services.length > 0 || learningComplete)) ||
    currentStep === 'target' ||
    currentStep === 'traffic' ||
    currentStep === 'run'

  const handleLearningComplete = () => {
    setLearningComplete(true)
    setCurrentStep('mode')
  }

  // A targetless service skips the traffic step.
  const steps = CONFIG_STEPS.filter(
    (step) => step !== 'traffic' || !draft.targetless,
  )
  const stepCount = steps.length + (learningComplete ? 1 : 0)
  const stepNumber =
    currentStep === 'learning'
      ? 1
      : steps.indexOf(currentStep) + 1 + (learningComplete ? 1 : 0)
  const modeLabel = modes.data?.find((mode) => mode.id === draft.mode)?.label
  const showModeBadge =
    currentStep !== 'learning' && currentStep !== 'export' && modeLabel

  return (
    <Dialog
      open={open}
      onOpenChange={(isOpen: boolean) => !isOpen && handleClose()}
    >
      {/* Only the step scrolls, so Back and Next stay in view. */}
      <DialogContent className="bg-card border-border flex max-h-[90vh] max-w-2xl flex-col gap-0 border shadow-xl [&>button]:hidden">
        <DialogHeader className="border-border -mx-6 shrink-0 border-b px-6 pb-4">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-3">
              <DialogTitle className="text-foreground text-xl font-semibold">
                {strings.wizard.stepTitles[currentStep]}
              </DialogTitle>
              {showModeBadge && (
                <Badge
                  variant="outline"
                  className="bg-primary/10 text-primary border-primary/20 text-xs font-medium"
                >
                  {modeLabel}
                </Badge>
              )}
            </div>
            <div className="flex items-center gap-4">
              {/* Step indicator */}
              <div className="flex items-center gap-2">
                {Array.from({ length: stepCount }, (_, index) => index + 1).map(
                  (n) => (
                    <div
                      key={n}
                      className={`h-2 rounded-full transition-all duration-300 ${
                        n === stepNumber
                          ? 'bg-primary w-6'
                          : n < stepNumber
                            ? 'bg-primary/50 w-2'
                            : 'bg-muted w-2'
                      }`}
                    />
                  ),
                )}
              </div>
              <DialogClose asChild>
                <button className="hover:bg-muted rounded-full p-2 transition-colors">
                  <X className="text-muted-foreground h-4 w-4" />
                </button>
              </DialogClose>
            </div>
          </div>
        </DialogHeader>
        <DialogDescription className="sr-only">
          {strings.wizard.dialogDescription}
        </DialogDescription>

        <div
          data-testid="wizard-step"
          className="-mx-6 min-h-0 flex-1 overflow-y-auto px-6 py-6"
        >
          {currentStep === 'learning' && (
            <LearningSteps
              onComplete={handleLearningComplete}
              onSkip={() => setCurrentStep('mode')}
            />
          )}
          {currentStep !== 'learning' && (
            <ErrorBoundary>
              {currentStep === 'mode' && <ModeStep />}
              {currentStep === 'target' && <TargetStep />}
              {currentStep === 'traffic' && <TrafficStep />}
              {currentStep === 'run' && <RunStep />}
              {currentStep === 'export' && (
                <ExportStep onAddService={() => setCurrentStep('mode')} />
              )}
            </ErrorBoundary>
          )}
        </div>

        {currentStep !== 'learning' && currentStep !== 'export' && (
          <DialogFooter className="border-border -mx-6 flex shrink-0 justify-between border-t px-6 pt-4 sm:justify-between">
            <div>
              {canGoBack && (
                <Button variant="outline" onClick={goBack} className="gap-2">
                  <ChevronLeft className="h-4 w-4" />
                  {strings.wizard.back}
                </Button>
              )}
            </div>
            <div>
              <Button
                onClick={goNext}
                disabled={!canGoNext}
                className="shadow-brand hover:shadow-brand-hover gap-2 text-white"
              >
                {currentStep === 'run' ? (
                  <>
                    <Plus className="h-4 w-4" />
                    {strings.wizard.addService}
                  </>
                ) : (
                  <>
                    {currentStep === 'mode'
                      ? strings.wizard.continue
                      : strings.wizard.next}
                    <ChevronRight className="h-4 w-4" />
                  </>
                )}
              </Button>
            </div>
          </DialogFooter>
        )}
      </DialogContent>
    </Dialog>
  )
}

export default Wizard
