import { useState, useEffect } from 'react'
import { ChevronLeft, ChevronRight, X } from 'lucide-react'
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
import { useConfigData, DefaultConfig } from './UserDataContext'
import { readBoilerplateType } from './JsonUtils'
import { strings } from '../strings'

const STEPS_WITH_LEARNING = 3

// Map internal mode names to display names
const modeDisplayNames: Record<string, string> = {
  steal: 'Filtering',
  mirror: 'Mirror',
  replace: 'Replace',
  custom: 'Custom',
}

const modeColors: Record<string, string> = {
  steal: 'bg-primary/10 text-primary border-primary/20',
  mirror: 'bg-primary/10 text-primary border-primary/20',
  replace: 'bg-primary/10 text-primary border-primary/20',
  custom: 'bg-primary/10 text-primary border-primary/20',
}

import BoilerplateStep from './steps/BoilerplateStep'
import ConfigTabs from './steps/ConfigTabs'
import LearningSteps from './steps/LearningSteps'
import ErrorBoundary from './ErrorBoundary'

interface WizardProps {
  open: boolean
  onClose: () => void
  startWithLearning?: boolean
}

type WizardStep = 'boilerplate' | 'config' | 'learning'

type ConfigTab = 'target' | 'network' | 'export'

const Wizard = ({ open, onClose, startWithLearning = false }: WizardProps) => {
  const { config, setConfig } = useConfigData()
  const [currentStep, setCurrentStep] = useState<WizardStep>(
    startWithLearning ? 'learning' : 'boilerplate',
  )
  const [learningComplete, setLearningComplete] = useState(false)
  const [currentTab, setCurrentTab] = useState<ConfigTab>('target')
  const [canAdvanceTab, setCanAdvanceTab] = useState(false)

  // Reset to correct step when wizard opens
  useEffect(() => {
    if (open) {
      setCurrentStep(startWithLearning ? 'learning' : 'boilerplate')
      setCurrentTab('target')
      setLearningComplete(false)
    }
  }, [open, startWithLearning])

  const handleClose = () => {
    setConfig(DefaultConfig)
    setCurrentStep(startWithLearning ? 'learning' : 'boilerplate')
    setLearningComplete(false)
    setCurrentTab('target')
    onClose()
  }

  const goToConfig = () => {
    setCurrentStep('boilerplate')
  }

  const goFromBoilerplate = () => {
    setCurrentStep('config')
    setCurrentTab('target')
  }

  const goBack = () => {
    if (currentStep === 'config') {
      if (currentTab === 'target') {
        setCurrentStep('boilerplate')
      } else if (currentTab === 'network') {
        setCurrentTab('target')
      } else {
        setCurrentTab('network')
      }
    } else if (currentStep === 'boilerplate' && learningComplete) {
      setCurrentStep('learning')
    }
  }

  const goNext = () => {
    if (currentStep === 'config') {
      if (currentTab === 'target') {
        setCurrentTab('network')
      } else if (currentTab === 'network') {
        setCurrentTab('export')
      }
    }
  }

  const handleLearningComplete = () => {
    setLearningComplete(true)
    setCurrentStep('boilerplate')
  }

  const boilerplateType = readBoilerplateType(config)
  const isTargetConfigView = currentStep === 'config' && currentTab === 'target'

  const getStepInfo = () => {
    switch (currentStep) {
      case 'learning':
        return { title: 'Learn mirrord', step: 1, total: STEPS_WITH_LEARNING }
      case 'boilerplate':
        return {
          title: 'Choose Mode',
          step: learningComplete ? 2 : 1,
          total: learningComplete ? STEPS_WITH_LEARNING : 2,
        }
      case 'config':
        return {
          title: 'Configure',
          step: learningComplete ? STEPS_WITH_LEARNING : 2,
          total: learningComplete ? STEPS_WITH_LEARNING : 2,
        }
      default:
        return { title: '', step: 0, total: 0 }
    }
  }

  const stepInfo = getStepInfo()

  return (
    <Dialog
      open={open}
      onOpenChange={(isOpen: boolean) => !isOpen && handleClose()}
    >
      <DialogContent
        className={`bg-card border-border max-h-[90vh] max-w-2xl overflow-y-auto border shadow-xl [&>button]:hidden ${
          isTargetConfigView ? 'h-[min(90vh,750px)]' : ''
        }`}
      >
        <DialogHeader className="border-border -mx-6 border-b px-6 pb-4">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-3">
              <DialogTitle className="text-foreground text-xl font-semibold">
                {stepInfo.title}
              </DialogTitle>
              {currentStep !== 'learning' && boilerplateType !== 'custom' && (
                <Badge
                  variant="outline"
                  className={`text-xs font-medium capitalize ${modeColors[boilerplateType] ?? modeColors['custom']}`}
                >
                  {modeDisplayNames[boilerplateType] ?? boilerplateType}
                </Badge>
              )}
            </div>
            <div className="flex items-center gap-4">
              {/* Step indicator */}
              <div className="flex items-center gap-2">
                {Array.from(
                  { length: stepInfo.total },
                  (_, index) => index + 1,
                ).map((stepNumber) => (
                  <div
                    key={stepNumber}
                    className={`h-2 rounded-full transition-all duration-300 ${
                      stepNumber === stepInfo.step
                        ? 'bg-primary w-6'
                        : stepNumber < stepInfo.step
                          ? 'bg-primary/50 w-2'
                          : 'bg-muted w-2'
                    }`}
                  />
                ))}
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

        <div className="py-6">
          {currentStep === 'learning' && (
            <LearningSteps
              onComplete={handleLearningComplete}
              onSkip={goToConfig}
            />
          )}
          {currentStep === 'boilerplate' && <BoilerplateStep />}
          {currentStep === 'config' && (
            <ErrorBoundary>
              <ConfigTabs
                currentTab={currentTab}
                onTabChange={setCurrentTab}
                onCanAdvanceChange={setCanAdvanceTab}
              />
            </ErrorBoundary>
          )}
        </div>

        {currentStep !== 'learning' && (
          <DialogFooter className="border-border -mx-6 flex justify-between border-t px-6 pt-4 sm:justify-between">
            {currentStep === 'boilerplate' && (
              <>
                <div>
                  {learningComplete && (
                    <Button
                      variant="outline"
                      onClick={goBack}
                      className="gap-2"
                    >
                      <ChevronLeft className="h-4 w-4" />
                      {strings.wizard.back}
                    </Button>
                  )}
                </div>
                <div>
                  <Button
                    onClick={goFromBoilerplate}
                    className="shadow-brand hover:shadow-brand-hover gap-2 text-white"
                  >
                    {strings.wizard.continue}
                    <ChevronRight className="h-4 w-4" />
                  </Button>
                </div>
              </>
            )}

            {currentStep === 'config' && (
              <>
                <Button variant="outline" onClick={goBack} className="gap-2">
                  <ChevronLeft className="h-4 w-4" />
                  {strings.wizard.back}
                </Button>
                {currentTab !== 'export' && canAdvanceTab && (
                  <Button
                    onClick={goNext}
                    className="shadow-brand hover:shadow-brand-hover gap-2 text-white"
                  >
                    {strings.wizard.next}
                    <ChevronRight className="h-4 w-4" />
                  </Button>
                )}
                {(currentTab === 'export' || !canAdvanceTab) && <div />}
              </>
            )}
          </DialogFooter>
        )}
      </DialogContent>
    </Dialog>
  )
}

export default Wizard
