import { useState } from 'react'
import { ChevronLeft, ChevronRight, CheckCircle } from 'lucide-react'
import { Button, Card, CardContent, CardFooter } from '@metalbear/ui'
import mirrordArchitecture from '../../assets/mirrord-architecture.svg'
import flowDiagram from '../../assets/flow-diagram.png'

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
          <strong>mirrord</strong> lets you run your local process in the context of a Kubernetes
          cluster. Instead of deploying your code to test it, you can develop and debug locally
          while connected to your cloud environment.
        </p>
        <p className="text-muted-foreground text-sm leading-relaxed">
          This means faster development cycles, real environment testing, and no need to set up
          complex local infrastructure.
        </p>
      </div>
    ),
  },
  {
    title: 'How does it work?',
    content: (
      <div className="space-y-4">
        <p className="text-foreground text-sm leading-relaxed">
          mirrord intercepts system calls from your local process and forwards them to a remote pod
          in your Kubernetes cluster.
        </p>
        <img
          src={mirrordArchitecture}
          alt="mirrord architecture"
          className="border-border w-full rounded-lg border"
        />
        <p className="text-muted-foreground text-sm leading-relaxed">
          Your local process sees the remote file system, environment variables, and network traffic
          as if it were running in the cluster.
        </p>
      </div>
    ),
  },
  {
    title: 'Traffic Modes',
    content: (
      <div className="space-y-4">
        <p className="text-foreground text-sm leading-relaxed">
          mirrord supports three ways to handle incoming traffic:
        </p>
        <ul className="space-y-3">
          <li className="flex gap-3 text-sm">
            <span className="bg-primary mt-1.5 h-2 w-2 flex-shrink-0 rounded-full" />
            <div>
              <strong className="text-foreground">Mirror</strong>
              <span className="text-muted-foreground">
                {' '}
                — Copy traffic without affecting the remote service
              </span>
            </div>
          </li>
          <li className="flex gap-3 text-sm">
            <span className="bg-primary mt-1.5 h-2 w-2 flex-shrink-0 rounded-full" />
            <div>
              <strong className="text-foreground">Steal (Filter)</strong>
              <span className="text-muted-foreground">
                {' '}
                — Redirect specific traffic based on headers or paths
              </span>
            </div>
          </li>
          <li className="flex gap-3 text-sm">
            <span className="bg-primary mt-1.5 h-2 w-2 flex-shrink-0 rounded-full" />
            <div>
              <strong className="text-foreground">Replace</strong>
              <span className="text-muted-foreground">
                {' '}
                — Fully replace the remote service with your local process
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
          With mirrord, your development workflow becomes:
        </p>
        <img
          src={flowDiagram}
          alt="Development flow"
          className="border-border w-full rounded-lg border"
        />
        <ol className="space-y-2 text-sm">
          <li className="flex gap-2">
            <span className="text-primary font-semibold">1.</span>
            <span className="text-foreground">Write code locally</span>
          </li>
          <li className="flex gap-2">
            <span className="text-primary font-semibold">2.</span>
            <span className="text-foreground">Run with mirrord to test in cluster context</span>
          </li>
          <li className="flex gap-2">
            <span className="text-primary font-semibold">3.</span>
            <span className="text-foreground">Debug and iterate instantly</span>
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
          mirrord uses a JSON configuration file to define how it connects to your cluster and
          handles traffic.
        </p>
        <div className="bg-muted rounded-lg p-4 font-mono text-xs">
          <pre className="text-foreground overflow-x-auto">{`{
  "target": {
    "path": "deployment/my-app",
    "namespace": "default"
  },
  "feature": {
    "network": {
      "incoming": { "mode": "steal" }
    }
  }
}`}</pre>
        </div>
        <p className="text-muted-foreground text-sm leading-relaxed">
          This wizard will help you create this configuration file step by step.
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
          You now understand the basics of mirrord!
        </p>
        <p className="text-muted-foreground text-sm leading-relaxed">
          Let's create your configuration file. Click "Start Configuration" to begin selecting your
          target and traffic mode.
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
          {steps.map((_, index) => (
            <button
              key={index}
              onClick={() => setCurrentStep(index)}
              className={`h-1.5 rounded-full transition-all duration-300 hover:opacity-80 ${index === currentStep ? 'bg-primary w-6' : 'bg-muted w-1.5'} ${index < currentStep ? 'bg-primary/50' : ''} `}
            />
          ))}
        </div>

        {/* Step title */}
        <h3 className="text-foreground text-center text-lg font-semibold">{step.title}</h3>

        {/* Step content */}
        <div>{step.content}</div>
      </CardContent>

      <CardFooter className="flex items-center justify-between">
        <div>
          {!isFirstStep && (
            <Button variant="outline" onClick={prev} className="gap-2">
              <ChevronLeft className="h-4 w-4" />
              Previous
            </Button>
          )}
        </div>
        <div className="flex items-center gap-3">
          {!isLastStep && (
            <Button variant="ghost" onClick={onSkip} className="text-muted-foreground">
              Skip
            </Button>
          )}
          <Button onClick={next} className="shadow-brand hover:shadow-brand-hover gap-2 text-white">
            {isLastStep ? 'Start Configuration' : 'Next'}
            {!isLastStep && <ChevronRight className="h-4 w-4" />}
          </Button>
        </div>
      </CardFooter>
    </Card>
  )
}

export default LearningSteps
