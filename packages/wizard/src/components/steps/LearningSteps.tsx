import { useState } from 'react'
import { ChevronLeft, ChevronRight, CheckCircle } from 'lucide-react'
import { Button, Card, CardContent, CardFooter } from '@metalbear/ui'
import mirrordArchitecture from '../../assets/mirrord-architecture.svg'
import flowDiagram from '../../assets/flow-diagram.png'
import { strings } from '../../strings'

interface LearningStepsProps {
  onComplete: () => void
  onSkip: () => void
}

interface Step {
  title: string
  content: React.ReactNode
}

const steps: Step[] = [
  {
    title: 'What is mirrord?',
    content: (
      <div className="space-y-4">
        <p className="text-foreground text-sm leading-relaxed">
          <strong>{strings.learningSteps.whatIsLead}</strong>{' '}
          {strings.learningSteps.whatIsBody}
        </p>
        <p className="text-muted-foreground text-sm leading-relaxed">
          {strings.learningSteps.whatIsBody2}
        </p>
      </div>
    ),
  },
  {
    title: 'How does it work?',
    content: (
      <div className="space-y-4">
        <p className="text-foreground text-sm leading-relaxed">
          {strings.learningSteps.howBody}
        </p>
        <img
          src={mirrordArchitecture}
          alt="mirrord architecture"
          className="border-border w-full rounded-lg border"
        />
        <p className="text-muted-foreground text-sm leading-relaxed">
          {strings.learningSteps.howBody2}
        </p>
      </div>
    ),
  },
  {
    title: 'Traffic Modes',
    content: (
      <div className="space-y-4">
        <p className="text-foreground text-sm leading-relaxed">
          {strings.learningSteps.modesBody}
        </p>
        <ul className="space-y-3">
          <li className="flex gap-3 text-sm">
            <span className="bg-primary mt-1.5 h-2 w-2 flex-shrink-0 rounded-full" />
            <div>
              <strong className="text-foreground">
                {strings.learningSteps.modeMirror}
              </strong>
              <span className="text-muted-foreground">
                {' '}
                {strings.learningSteps.modeMirrorDesc}
              </span>
            </div>
          </li>
          <li className="flex gap-3 text-sm">
            <span className="bg-primary mt-1.5 h-2 w-2 flex-shrink-0 rounded-full" />
            <div>
              <strong className="text-foreground">
                {strings.learningSteps.modeSteal}
              </strong>
              <span className="text-muted-foreground">
                {' '}
                {strings.learningSteps.modeStealDesc}
              </span>
            </div>
          </li>
          <li className="flex gap-3 text-sm">
            <span className="bg-primary mt-1.5 h-2 w-2 flex-shrink-0 rounded-full" />
            <div>
              <strong className="text-foreground">
                {strings.learningSteps.modeReplace}
              </strong>
              <span className="text-muted-foreground">
                {' '}
                {strings.learningSteps.modeReplaceDesc}
              </span>
            </div>
          </li>
        </ul>
      </div>
    ),
  },
  {
    title: 'The Development Loop',
    content: (
      <div className="space-y-4">
        <p className="text-foreground text-sm leading-relaxed">
          {strings.learningSteps.loopBody}
        </p>
        <img
          src={flowDiagram}
          alt="Development flow"
          className="border-border w-full rounded-lg border"
        />
        <ol className="space-y-2 text-sm">
          <li className="flex gap-2">
            <span className="text-primary font-semibold">
              {strings.learningSteps.loopStepOne}
            </span>
            <span className="text-foreground">
              {strings.learningSteps.loopStepOneText}
            </span>
          </li>
          <li className="flex gap-2">
            <span className="text-primary font-semibold">
              {strings.learningSteps.loopStepTwo}
            </span>
            <span className="text-foreground">
              {strings.learningSteps.loopStepTwoText}
            </span>
          </li>
          <li className="flex gap-2">
            <span className="text-primary font-semibold">
              {strings.learningSteps.loopStepThree}
            </span>
            <span className="text-foreground">
              {strings.learningSteps.loopStepThreeText}
            </span>
          </li>
        </ol>
      </div>
    ),
  },
  {
    title: 'Configuration File',
    content: (
      <div className="space-y-4">
        <p className="text-foreground text-sm leading-relaxed">
          {strings.learningSteps.configBody}
        </p>
        <div className="bg-muted rounded-lg p-4 font-mono text-xs">
          <pre className="text-foreground overflow-x-auto">
            {strings.learningSteps.configExample}
          </pre>
        </div>
        <p className="text-muted-foreground text-sm leading-relaxed">
          {strings.learningSteps.configBody2}
        </p>
      </div>
    ),
  },
  {
    title: 'Ready to Configure!',
    content: (
      <div className="space-y-4 text-center">
        <div className="bg-primary/10 mx-auto flex h-16 w-16 items-center justify-center rounded-full">
          <CheckCircle className="text-primary h-8 w-8" />
        </div>
        <p className="text-foreground text-base font-medium">
          {strings.learningSteps.readyTitle}
        </p>
        <p className="text-muted-foreground text-sm leading-relaxed">
          {strings.learningSteps.readyBody}
        </p>
      </div>
    ),
  },
]

const LearningSteps = ({ onComplete, onSkip }: LearningStepsProps) => {
  const [currentStep, setCurrentStep] = useState(0)
  const isLastStep = currentStep === steps.length - 1
  const isFirstStep = currentStep === 0
  const step = steps[currentStep]

  const next = () => {
    if (isLastStep) {
      onComplete()
    } else {
      setCurrentStep((s) => s + 1)
    }
  }

  const prev = () => {
    if (!isFirstStep) {
      setCurrentStep((s) => s - 1)
    }
  }

  if (!step) return null

  return (
    <Card className="border-border">
      <CardContent className="space-y-5 pt-6">
        {/* Progress indicator */}
        <div className="flex items-center justify-center gap-1.5">
          {steps.map((progressStep, index) => (
            <button
              key={progressStep.title}
              onClick={() => setCurrentStep(index)}
              className={`h-1.5 rounded-full transition-all duration-300 hover:opacity-80 ${index === currentStep ? 'bg-primary w-6' : 'bg-muted w-1.5'} ${index < currentStep ? 'bg-primary/50' : ''} `}
            />
          ))}
        </div>

        {/* Step title */}
        <h3 className="text-foreground text-center text-lg font-semibold">
          {step.title}
        </h3>

        {/* Step content */}
        <div>{step.content}</div>
      </CardContent>

      <CardFooter className="flex items-center justify-between">
        <div>
          {!isFirstStep && (
            <Button variant="outline" onClick={prev} className="gap-2">
              <ChevronLeft className="h-4 w-4" />
              {strings.learningSteps.previous}
            </Button>
          )}
        </div>
        <div className="flex items-center gap-3">
          {!isLastStep && (
            <Button
              variant="ghost"
              onClick={onSkip}
              className="text-muted-foreground"
            >
              {strings.learningSteps.skip}
            </Button>
          )}
          <Button
            onClick={next}
            className="shadow-brand hover:shadow-brand-hover gap-2 text-white"
          >
            {isLastStep ? 'Start Configuration' : 'Next'}
            {!isLastStep && <ChevronRight className="h-4 w-4" />}
          </Button>
        </div>
      </CardFooter>
    </Card>
  )
}

export default LearningSteps
