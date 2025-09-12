import { useState } from "react";
import { ArrowRight, Zap, BookOpen, ChevronLeft, ChevronRight } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { ConfigWizard } from "@/components/ConfigWizard";
import { useNavigate } from "react-router-dom";
import mirroredArchitecture from "@/assets/mirrord-architecture.svg";
import mirrordLogo from "@/assets/mirrord-logo.png";
interface Config {
  id: string;
  name: string;
  target: string;
  service: string;
  namespace: string;
  isActive: boolean;
  createdAt: string;
  fileSystem: {
    enabled: boolean;
    mode: "read" | "write" | "local";
    rules: Array<{
      mode: "read" | "write" | "local";
      filter: string;
    }>;
  };
  network: {
    incoming: {
      enabled: boolean;
      mode: "steal" | "mirror";
      httpFilter: Array<{
        type: "header" | "method" | "content" | "path";
        value: string;
      }>;
      filterOperator: "AND" | "OR";
      ports: Array<{
        remote: string;
        local: string;
      }>;
    };
    outgoing: {
      enabled: boolean;
      protocol: "tcp" | "udp" | "both";
      filter: string;
      filterTarget: "remote" | "local";
    };
    dns: {
      enabled: boolean;
      filter: string;
    };
  };
  environment: {
    enabled: boolean;
    include: string;
    exclude: string;
    override: string;
  };
  agent: {
    scaledown: boolean;
    copyTarget: boolean;
  };
}
const Onboarding = () => {
  const [currentStep, setCurrentStep] = useState("welcome");
  const [wizardStep, setWizardStep] = useState(1);
  const [showWizard, setShowWizard] = useState(false);
  const navigate = useNavigate();
  const handleConfigSave = (config: Partial<Config>) => {
    const existingConfigs = JSON.parse(localStorage.getItem('mirrord-configs') || '[]');
    const newConfig = {
      ...config,
      id: Date.now().toString(),
      service: config.target?.split(' ')[0] || 'my-service',
      createdAt: new Date().toISOString().split('T')[0]
    };
    localStorage.setItem('mirrord-configs', JSON.stringify([...existingConfigs, newConfig]));
    localStorage.setItem('mirrord-onboarding-completed', 'true');
    navigate('/dashboard');
  };
  const handleSkipToConfig = () => {
    setCurrentStep("wizard-7");
    setShowWizard(true);
  };
  const handleLearnFirst = () => {
    setCurrentStep("wizard-1");
    setWizardStep(1);
  };
  const nextWizardStep = () => {
    if (wizardStep < 7) {
      const nextStep = wizardStep + 1;
      setWizardStep(nextStep);
      setCurrentStep(`wizard-${nextStep}`);
      if (nextStep === 7) {
        setShowWizard(true);
      }
    }
  };
  const prevWizardStep = () => {
    if (wizardStep > 1) {
      const prevStep = wizardStep - 1;
      setWizardStep(prevStep);
      setCurrentStep(`wizard-${prevStep}`);
      setShowWizard(false);
    } else {
      setCurrentStep("welcome");
    }
  };
  const WizardHeader = ({
    step,
    title,
    subtitle
  }: {
    step: number;
    title: string;
    subtitle: string;
  }) => <div className="text-center mb-3 sm:mb-4">
      <div className="flex items-center justify-center gap-2 mb-2">
        <Zap className="h-4 w-4 sm:h-5 sm:w-5 text-primary" />
        <span className="text-base sm:text-lg font-bold bg-gradient-primary bg-clip-text text-transparent">
          mirrord
        </span>
      </div>
      <Badge variant="secondary" className="text-xs mb-2">Step {step} of 7</Badge>
      <h1 className="text-lg sm:text-xl font-bold mb-2 text-foreground">
        {title}
      </h1>
      <p className="text-muted-foreground text-xs sm:text-sm max-w-lg mx-auto px-4">
        {subtitle}
      </p>
    </div>;
  const WizardNavigation = ({
    showNext = true,
    showPrev = true,
    nextLabel = "Next"
  }: {
    showNext?: boolean;
    showPrev?: boolean;
    nextLabel?: string;
  }) => <div className="flex justify-between items-center mt-3 sm:mt-4 px-4 sm:px-0">
      {showPrev ? <Button variant="outline" size="sm" onClick={prevWizardStep} className="flex items-center gap-2">
          <ChevronLeft className="h-4 w-4" />
          <span className="hidden sm:inline">Back</span>
        </Button> : <div></div>}
      {showNext && <Button size="sm" onClick={nextWizardStep} className="bg-gradient-primary flex items-center gap-2">
          <span>{nextLabel}</span>
          <ChevronRight className="h-4 w-4" />
        </Button>}
    </div>;
  if (currentStep === "welcome") {
    return <div className="min-h-screen w-full bg-background flex items-center justify-center p-4">
        <div className="max-w-4xl mx-auto">
          <div className="text-center mb-8 sm:mb-12">
            <div className="flex items-center justify-center mb-6">
              <img src={mirrordLogo} alt="mirrord" className="h-16 w-auto sm:h-20" />
            </div>
            <Badge variant="secondary" className="text-xs mb-6">v3.86.0</Badge>
            
            <h1 className="text-2xl sm:text-3xl font-bold mb-4 text-foreground">
              Welcome to mirrord! 👋
            </h1>
            <p className="text-muted-foreground text-sm sm:text-lg max-w-2xl mx-auto">
              Run your local code in your Kubernetes cluster without the complexity of deployments. Let's get you started!
            </p>
          </div>

          <div className="grid gap-6 sm:gap-8 md:grid-cols-2 max-w-4xl mx-auto">
            <Card className="bg-gradient-card border-border/50 hover:shadow-glow transition-all duration-300">
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <BookOpen className="h-5 w-5" />
                  Learn About mirrord First
                </CardTitle>
                <CardDescription>
                  Understand how mirrord works and explore the overview before creating your first configuration
                </CardDescription>
              </CardHeader>
              <CardContent>
                <Button onClick={handleLearnFirst} className="w-full">
                  Show Me How It Works
                  <ArrowRight className="h-4 w-4 ml-2" />
                </Button>
              </CardContent>
            </Card>

            <Card className="bg-gradient-card border-border/50 hover:shadow-glow transition-all duration-300">
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <Zap className="h-5 w-5" />
                  Skip to Configuration
                </CardTitle>
                <CardDescription>
                  Jump directly to creating your mirrord.json configuration file
                </CardDescription>
              </CardHeader>
              <CardContent>
                <Button variant="outline" onClick={handleSkipToConfig} className="w-full">
                  Create Configuration Now
                  <ArrowRight className="h-4 w-4 ml-2" />
                </Button>
              </CardContent>
            </Card>
          </div>
        </div>
      </div>;
  }

  // Wizard Step 1: mirrord introduction
  if (currentStep === "wizard-1") {
    return <div className="min-h-screen w-full bg-background flex items-center justify-center py-8">
        <div className="max-w-3xl mx-auto px-4">
          <WizardHeader step={1} title="What is mirrord?" subtitle="Revolutionary Kubernetes development without deployment overhead" />
          
          <div className="space-y-4 max-w-3xl mx-auto">
            <Card className="bg-gradient-card border-border/50 shadow-glow">
              <CardHeader className="pb-3">
                <CardTitle className="text-lg">What is mirrord?</CardTitle>
              </CardHeader>
              <CardContent className="pt-0">
                <p className="text-foreground text-sm leading-relaxed mb-4">
                  mirrord lets you run your local code in the context of your cloud environment. Unlike traditional development tools, you can test your code with real cloud dependencies, data, and configuration without deploying anything.
                </p>
                
                <div className="bg-muted/30 rounded-lg p-4">
                  <h4 className="font-semibold mb-3 text-accent-foreground text-sm">Why mirrord vs. Other Solutions?</h4>
                  <div className="grid gap-2">
                    <div className="flex items-start gap-2">
                      <span className="text-red-500 mt-0.5 text-sm">×</span>
                      <span className="text-muted-foreground text-xs"><strong>Port forwarding:</strong> Only gives you database access, no real environment context</span>
                    </div>
                    <div className="flex items-start gap-2">
                      <span className="text-red-500 mt-0.5 text-sm">×</span>
                      <span className="text-muted-foreground text-xs"><strong>Local clusters:</strong> Resource-heavy, doesn't match production environment</span>
                    </div>
                    <div className="flex items-start gap-2">
                      <span className="text-red-500 mt-0.5 text-sm">×</span>
                      <span className="text-muted-foreground text-xs"><strong>Deploy to dev cluster:</strong> Slow feedback loop, complex CI/CD setup</span>
                    </div>
                    <div className="flex items-start gap-2">
                      <span className="text-green-500 mt-0.5 text-sm">✓</span>
                      <span className="text-foreground text-xs"><strong>mirrord:</strong> Run locally with full cloud context in seconds</span>
                    </div>
                  </div>
                </div>
              </CardContent>
            </Card>
            
            <WizardNavigation nextLabel="Continue" />
          </div>
        </div>
      </div>;
  }

  // Wizard Step 2: How mirrord works
  if (currentStep === "wizard-2") {
    return <div className="min-h-screen w-full bg-background flex items-center justify-center py-8">
        <div className="max-w-3xl mx-auto px-4">
          <WizardHeader step={2} title="How mirrord Works" subtitle="Understanding the architecture and components" />
          
          <div className="space-y-4 max-w-3xl mx-auto">
            <Card className="bg-gradient-card border-border/50 shadow-glow">
              <CardHeader className="pb-3">
                <CardTitle className="text-lg">Architecture Overview</CardTitle>
              </CardHeader>
              <CardContent className="pt-0">
                <p className="text-muted-foreground mb-4 leading-relaxed text-sm">
                  mirrord runs in two places - in the memory of your local process (mirrord-layer), and as a pod in your cloud environment (mirrord-agent).
                </p>
                
                <div className="bg-muted/30 rounded-lg p-4 mb-4">
                  <h5 className="font-semibold mb-3 text-accent-foreground text-sm">mirrord - Basic Architecture</h5>
                  <p className="text-muted-foreground mb-3 text-xs">
                    When you start your local process with mirrord, it creates a pod in your cloud environment, which listens in on the pod you've passed as an argument. mirrord-layer then does the following:
                  </p>
                  
                  <div className="ml-3 space-y-2">
                    <p className="font-medium text-foreground text-xs">Override the process' syscalls to:</p>
                    <ul className="space-y-1 ml-3 text-muted-foreground text-xs">
                      <li>• Listen to incoming traffic from the agent, instead of local sockets</li>
                      <li>• Intercept outgoing traffic and send it out from the remote pod, instead of locally</li>
                      <li>• Read and write files to the remote file system</li>
                      <li>• Merge the process' environment variables with those of the remote pod</li>
                    </ul>
                  </div>
                </div>
                
                <div className="bg-background border border-border rounded-lg p-2">
                  <img src={mirroredArchitecture} alt="mirrord Architecture Diagram" className="w-full max-w-full mx-auto rounded" />
                </div>
              </CardContent>
            </Card>
            
            <WizardNavigation nextLabel="Continue to Filtering Mode" />
          </div>
        </div>
      </div>;
  }

  // Wizard Step 3: Steal mode
  if (currentStep === "wizard-3") {
    return <div className="min-h-screen w-full bg-background flex items-center justify-center py-8">
        <div className="max-w-3xl mx-auto px-4">
          <WizardHeader step={3} title="Filtering Mode" subtitle="Your local service only handles matching traffic based on HTTP Headers/Path (This includes gRPC). This allows you to control what flows in the system you want to affect." />
          
          <div className="space-y-4 max-w-3xl mx-auto">
            <Card className="bg-gradient-card border-border/50 shadow-glow">
              <CardHeader className="pb-3">
                <CardTitle className="text-lg">How Filtering Mode Works</CardTitle>
              </CardHeader>
              <CardContent className="pt-0">
                <div className="bg-muted/30 rounded-lg p-4">
                  <h5 className="font-semibold mb-3 text-foreground text-sm">Process Overview:</h5>
                  <ol className="space-y-2 text-muted-foreground list-decimal ml-4 mb-3 text-xs">
                    <li>Remote traffic is stolen selectively</li>
                    <li>Remote resources such as databases and other services are used.</li>
                  </ol>
                  <p className="text-muted-foreground mb-3 text-xs">
                    Queues are still shared unless using queue splitting as well.
                  </p>
                  <p className="font-medium text-primary text-xs">
                    Best for: Testing against shared environment
                  </p>
                </div>
              </CardContent>
            </Card>
            
            <WizardNavigation nextLabel="Learn About Mirror Mode" />
          </div>
        </div>
      </div>;
  }

  // Wizard Step 4: Mirror mode
  if (currentStep === "wizard-4") {
    return <div className="min-h-screen w-full bg-background flex items-center justify-center py-8">
        <div className="max-w-3xl mx-auto px-4">
          <WizardHeader step={4} title="Mirror mode" subtitle="Mirror traffic to your local service without disruption" />
          
          <div className="space-y-4 max-w-3xl mx-auto">
            <Card className="bg-gradient-card border-border/50 shadow-glow">
              <CardHeader className="pb-3">
                <CardTitle className="text-lg">How Mirror Mode Works</CardTitle>
              </CardHeader>
              <CardContent className="pt-0">
                <div className="bg-muted/30 rounded-lg p-4">
                  <p className="text-muted-foreground mb-3 leading-relaxed text-xs">
                    Your local service receives a copy of the incoming traffic sent to the targeted remote service. The response will come from the remote service and your local service will handle the same request but it's response will be discarded. By default, you will connect to the remote dependencies so you can have side effects (i.e writing to same database)
                  </p>
                  <p className="font-medium text-primary text-xs">
                    Best for: Debugging existing traffic, understanding how APIs behave without deploying/changing logic in the environment.
                  </p>
                </div>
              </CardContent>
            </Card>
            
            <WizardNavigation nextLabel="Learn About Replace Mode" />
          </div>
        </div>
      </div>;
  }

  // Wizard Step 5: Replace mode
  if (currentStep === "wizard-5") {
    return <div className="min-h-screen w-full bg-background flex items-center justify-center py-8">
        <div className="max-w-3xl mx-auto px-4">
          <WizardHeader step={5} title="Replace Mode" subtitle="Completely substitute the remote service" />
          
          <div className="space-y-4 max-w-3xl mx-auto">
            <Card className="bg-gradient-card border-border/50 shadow-glow">
              <CardHeader className="pb-3">
                <CardTitle className="text-lg">How Replace Mode Works</CardTitle>
              </CardHeader>
              <CardContent className="pt-0">
                <div className="bg-muted/30 rounded-lg p-4">
                  <p className="text-muted-foreground leading-relaxed text-xs">
                    Replace mode completely substitutes the remote service's traffic and queues. Useful when Queue Splitting isn't available and you want to work against queues.
                  </p>
                </div>
              </CardContent>
            </Card>
            
            <WizardNavigation nextLabel="See Feedback Loop" />
          </div>
        </div>
      </div>;
  }

  // Wizard Step 6: Feedback loop diagram
  if (currentStep === "wizard-6") {
    return <div className="min-h-screen w-full bg-background flex items-center justify-center py-8">
        <div className="max-w-3xl mx-auto px-4">
          <WizardHeader step={6} title="Development Feedback Loop" subtitle="See how mirrord accelerates your development cycle" />
          
          <div className="space-y-4 max-w-3xl mx-auto">
            <Card className="bg-gradient-card border-border/50 shadow-glow">
              <CardHeader className="pb-3">
                <CardTitle className="text-lg">Fast Development Feedback Loop</CardTitle>
              </CardHeader>
              <CardContent className="pt-0">
                <p className="text-foreground text-sm leading-relaxed mb-4 text-center">
                  mirrord creates a fast feedback loop that dramatically speeds up cloud-native development by eliminating the need for deployments during development.
                </p>
                
                <div className="bg-background border border-border rounded-lg p-2 mb-4">
                  <img src="/lovable-uploads/0514df82-cea8-46c8-bfb5-05db3e6778e2.png" alt="Development Feedback Loop with mirrord" className="w-full max-w-full mx-auto rounded" />
                </div>
              </CardContent>
            </Card>
            
            <WizardNavigation nextLabel="Create Configuration" />
          </div>
        </div>
      </div>;
  }
  return <>
      {showWizard && <ConfigWizard isOpen={showWizard} onClose={() => {
      setShowWizard(false);
      setCurrentStep("welcome");
    }} onSave={handleConfigSave} existingConfigs={[]} />}
    </>;
};
export default Onboarding;