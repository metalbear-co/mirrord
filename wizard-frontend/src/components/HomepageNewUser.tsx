import { useState } from "react";
import { ArrowRight, Zap, BookOpen, ChevronLeft, ChevronRight, Plus, Filter, Copy, Repeat, Server } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { ConfigWizard } from "@/components/ConfigWizard";
import { BoilerplateCard } from "@/components/BoilerplateCard";
import { WizardStep } from "@/components/Wizard";
import { useNavigate } from "react-router-dom";
import mirroredArchitecture from "@/assets/mirrord-architecture.svg";
import mirrordLogo from "@/assets/mirrord-logo.png";
import Panel from "./Panel";

const HomepageNewUser = () => {
  const isReturning = false;
  const titleCreateConfig = (
    <CardTitle className="flex items-center gap-2">
      <Zap className="h-5 w-5" /> Skip to Configuration
    </CardTitle>
  );
  const titleLearn = (
    <CardTitle className="flex items-center gap-2">
      <BookOpen className="h-5 w-5" /> Learn About mirrord First
    </CardTitle>
  );

  const boilerplateStep: WizardStep = {
    id: "boilerplate-new-user",
    title: "mirrord configuration",
    content: (
      <div className="space-y-6">
        <div className="flex flex-col gap-2">
          <BoilerplateCard
            id="steal"
            title="Filtering mode"
            description="Suitable for scenarios where you want to see how your changes impact remote environment while reducing the impact radius"
            features={["steal mode", "selective traffic"]}
            icon={Filter}
            color="text-purple-500"
            selected={false}
            onClick={() => {}}
          />
          <BoilerplateCard
            id="mirror"
            title="Mirror mode"
            description="This is useful when you want the remote target to serve requests and you're okay with one request being handled twice"
            features={["mirror mode"]}
            icon={Copy}
            color="text-blue-500"
            selected={false}
            onClick={() => {}}
          />
          <BoilerplateCard
            id="replace"
            title="Replace mode"
            description="Suitable for scenarios where you have your own namespace/cluster and you're okay with replacing the remote service entirely. Note: Cannot replace pods, only other entities."
            features={["steal mode", "copy target", "scale down"]}
            icon={Repeat}
            color="text-orange-500"
            selected={false}
            onClick={() => {}}
          />
        </div>
      </div>
    )
  };

  const introStep: WizardStep = {
    id: "intro-new-user",
    title: "Welcome to mirrord Configuration",
    content: (
      <div className="space-y-6">
        <div className="text-center space-y-4">
          <div className="mx-auto w-24 h-24 bg-gradient-to-br from-primary to-primary/60 rounded-full flex items-center justify-center">
            <Server className="h-12 w-12 text-white" />
          </div>

          <div className="space-y-2">
            <h3 className="text-lg font-semibold">
              Get started with mirrord
            </h3>
            <p className="text-muted-foreground max-w-md mx-auto">
              Run local code like it's in your Kubernetes cluster
              without deploying it first. Get started by creating your
              first mirrord.json configuration.
            </p>
          </div>
        </div>
      </div>
    )
  };

  const explanationStep: WizardStep = {
    id: "explanation-new-user",
    title: "How mirrord Works",
    content: (
      <div className="space-y-6">
        <div className="text-center space-y-4 pt-4">
          <p className="text-sm text-muted-foreground">
            Skip this if you're already familiar with mirrord and just
            want to create a config file
          </p>
        </div>
      </div>
    )
  };

  const configStep: WizardStep = {
    id: "config-new-user",
    title: "Configuration Setup",
    content: (
      <div className="space-y-6">
        <div className="text-center space-y-4 pt-4">
          <p className="text-sm text-muted-foreground">
            Configure your mirrord settings using the tabs below
          </p>
        </div>
      </div>
    )
  };

  // Learning steps

  const [currentStep, setCurrentStep] = useState("welcome");
  const [wizardStep, setWizardStep] = useState(1);
  const [showWizard, setShowWizard] = useState(false);
  const navigate = useNavigate();
  const handleConfigSave = () => {
    // TODO: ??
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

  const learningStep1: WizardStep = {
    id: "wizard-1-intro",
    title: "What is mirrord?",
    content: (
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
      </div>
    )
  };

  const learningStep2: WizardStep = {
    id: "wizard-2-architecture",
    title: "How mirrord Works",
    content: (
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
      </div>
    )
  };

  const learningStep3: WizardStep = {
    id: "wizard-3-filtering",
    title: "Filtering Mode",
    content: (
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
      </div>
    )
  };

  const learningStep4: WizardStep = {
    id: "wizard-4-mirror",
    title: "Mirror mode",
    content: (
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
      </div>
    )
  };

  const learningStep5: WizardStep = {
    id: "wizard-5-replace",
    title: "Replace Mode",
    content: (
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
      </div>
    )
  };

  const learningStep6: WizardStep = {
    id: "wizard-6-feedback",
    title: "Development Feedback Loop",
    content: (
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
      </div>
    )
  };

  return (
    <div className="min-h-screen w-full bg-background flex items-center justify-center p-4">
      
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
        <div className="grid gap-6 sm:gap-8  md:grid-cols-2 max-w-4xl mx-auto">
          <Panel title={titleLearn} content={"Understand how mirrord works and explore the overview before creating your first configuration"} buttonText={"Show Me How It Works"} buttonColor={"purple"} steps={[]} isReturning={isReturning} />
          <Panel title={titleCreateConfig} content={"Jump directly to creating your mirrord.json configuration file"} buttonText={"Create Configuration Now"} buttonColor={"gray"} steps={[introStep, learningStep1, learningStep2, learningStep3, learningStep4, learningStep5, learningStep6, explanationStep, boilerplateStep, configStep]} isReturning={isReturning} />
        </div>
      </div>
    </div>
  );
};
export default HomepageNewUser;