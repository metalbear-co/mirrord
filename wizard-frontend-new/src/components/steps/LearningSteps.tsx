import { useState } from "react";
import { ChevronLeft, ChevronRight, CheckCircle } from "lucide-react";
import { Button } from "@metalbear/ui";
import mirrordArchitecture from "../../assets/mirrord-architecture.svg";
import flowDiagram from "../../assets/flow-diagram.png";

interface LearningStepsProps {
  onComplete: () => void;
  onSkip: () => void;
}

interface Step {
  title: string;
  content: React.ReactNode;
}

const steps: Step[] = [
  {
    title: "What is mirrord?",
    content: (
      <div className="space-y-4">
        <p className="text-sm text-[var(--foreground)] leading-relaxed">
          <strong>mirrord</strong> lets you run your local process in the context of a
          Kubernetes cluster. Instead of deploying your code to test it, you can
          develop and debug locally while connected to your cloud environment.
        </p>
        <p className="text-sm text-[var(--muted-foreground)] leading-relaxed">
          This means faster development cycles, real environment testing, and no
          need to set up complex local infrastructure.
        </p>
      </div>
    ),
  },
  {
    title: "How does it work?",
    content: (
      <div className="space-y-4">
        <p className="text-sm text-[var(--foreground)] leading-relaxed">
          mirrord intercepts system calls from your local process and forwards them
          to a remote pod in your Kubernetes cluster.
        </p>
        <img
          src={mirrordArchitecture}
          alt="mirrord architecture"
          className="w-full rounded-lg border border-[var(--border)]"
        />
        <p className="text-sm text-[var(--muted-foreground)] leading-relaxed">
          Your local process sees the remote file system, environment variables,
          and network traffic as if it were running in the cluster.
        </p>
      </div>
    ),
  },
  {
    title: "Traffic Modes",
    content: (
      <div className="space-y-4">
        <p className="text-sm text-[var(--foreground)] leading-relaxed">
          mirrord supports three ways to handle incoming traffic:
        </p>
        <ul className="space-y-3">
          <li className="flex gap-3 text-sm">
            <span className="w-2 h-2 rounded-full bg-primary mt-1.5 flex-shrink-0" />
            <div>
              <strong className="text-[var(--foreground)]">Mirror</strong>
              <span className="text-[var(--muted-foreground)]">
                {" "}— Copy traffic without affecting the remote service
              </span>
            </div>
          </li>
          <li className="flex gap-3 text-sm">
            <span className="w-2 h-2 rounded-full bg-primary mt-1.5 flex-shrink-0" />
            <div>
              <strong className="text-[var(--foreground)]">Steal (Filter)</strong>
              <span className="text-[var(--muted-foreground)]">
                {" "}— Redirect specific traffic based on headers or paths
              </span>
            </div>
          </li>
          <li className="flex gap-3 text-sm">
            <span className="w-2 h-2 rounded-full bg-primary mt-1.5 flex-shrink-0" />
            <div>
              <strong className="text-[var(--foreground)]">Replace</strong>
              <span className="text-[var(--muted-foreground)]">
                {" "}— Fully replace the remote service with your local process
              </span>
            </div>
          </li>
        </ul>
      </div>
    ),
  },
  {
    title: "The Development Loop",
    content: (
      <div className="space-y-4">
        <p className="text-sm text-[var(--foreground)] leading-relaxed">
          With mirrord, your development workflow becomes:
        </p>
        <img
          src={flowDiagram}
          alt="Development flow"
          className="w-full rounded-lg border border-[var(--border)]"
        />
        <ol className="space-y-2 text-sm">
          <li className="flex gap-2">
            <span className="text-primary font-semibold">1.</span>
            <span className="text-[var(--foreground)]">Write code locally</span>
          </li>
          <li className="flex gap-2">
            <span className="text-primary font-semibold">2.</span>
            <span className="text-[var(--foreground)]">Run with mirrord to test in cluster context</span>
          </li>
          <li className="flex gap-2">
            <span className="text-primary font-semibold">3.</span>
            <span className="text-[var(--foreground)]">Debug and iterate instantly</span>
          </li>
        </ol>
      </div>
    ),
  },
  {
    title: "Configuration File",
    content: (
      <div className="space-y-4">
        <p className="text-sm text-[var(--foreground)] leading-relaxed">
          mirrord uses a JSON configuration file to define how it connects to your
          cluster and handles traffic.
        </p>
        <div className="bg-[var(--muted)] rounded-lg p-4 font-mono text-xs">
          <pre className="text-[var(--foreground)] overflow-x-auto">{`{
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
        <p className="text-sm text-[var(--muted-foreground)] leading-relaxed">
          This wizard will help you create this configuration file step by step.
        </p>
      </div>
    ),
  },
  {
    title: "Ready to Configure!",
    content: (
      <div className="space-y-4 text-center">
        <div className="w-16 h-16 rounded-full bg-primary/10 flex items-center justify-center mx-auto">
          <CheckCircle className="h-8 w-8 text-primary" />
        </div>
        <p className="text-base font-medium text-[var(--foreground)]">
          You now understand the basics of mirrord!
        </p>
        <p className="text-sm text-[var(--muted-foreground)] leading-relaxed">
          Let's create your configuration file. Click "Start Configuration" to
          begin selecting your target and traffic mode.
        </p>
      </div>
    ),
  },
];

const LearningSteps = ({ onComplete, onSkip }: LearningStepsProps) => {
  const [currentStep, setCurrentStep] = useState(0);
  const isLastStep = currentStep === steps.length - 1;
  const isFirstStep = currentStep === 0;

  const next = () => {
    if (isLastStep) {
      onComplete();
    } else {
      setCurrentStep((s) => s + 1);
    }
  };

  const prev = () => {
    if (!isFirstStep) {
      setCurrentStep((s) => s - 1);
    }
  };

  return (
    <div className="space-y-5">
      {/* Progress indicator */}
      <div className="flex items-center justify-center gap-1.5">
        {steps.map((_, index) => (
          <button
            key={index}
            onClick={() => setCurrentStep(index)}
            className={`
              h-1.5 rounded-full transition-all duration-300 hover:opacity-80
              ${index === currentStep ? "w-6 bg-primary" : "w-1.5 bg-[var(--muted)]"}
              ${index < currentStep ? "bg-primary/50" : ""}
            `}
          />
        ))}
      </div>

      {/* Step title */}
      <h3 className="text-lg font-semibold text-[var(--foreground)] text-center">
        {steps[currentStep].title}
      </h3>

      {/* Step content */}
      <div>{steps[currentStep].content}</div>

      {/* Navigation */}
      <div className="flex items-center justify-between pt-4 border-t border-[var(--border)]">
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
            <Button variant="ghost" onClick={onSkip} className="text-[var(--muted-foreground)]">
              Skip
            </Button>
          )}
          <Button onClick={next} className="gap-2 shadow-brand hover:shadow-brand-hover">
            {isLastStep ? "Start Configuration" : "Next"}
            {!isLastStep && <ChevronRight className="h-4 w-4" />}
          </Button>
        </div>
      </div>
    </div>
  );
};

export default LearningSteps;
