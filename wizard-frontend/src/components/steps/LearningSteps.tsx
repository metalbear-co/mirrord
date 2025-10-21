import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { WizardStep } from "@/components/Wizard";
import mirroredArchitecture from "@/assets/mirrord-architecture.svg";

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
            mirrord lets you run your local code in the context of your cloud
            environment. Unlike traditional development tools, you can test your
            code with real cloud dependencies, data, and configuration without
            deploying anything.
          </p>

          <div className="bg-muted/30 rounded-lg p-4">
            <h4 className="font-semibold mb-3 text-accent-foreground text-sm">
              Why mirrord vs. Other Solutions?
            </h4>
            <div className="grid gap-2">
              <div className="flex items-start gap-2">
                <span className="text-red-500 mt-0.5 text-sm">×</span>
                <span className="text-muted-foreground text-xs">
                  <strong>Port forwarding:</strong> Only gives you database
                  access, no real environment context
                </span>
              </div>
              <div className="flex items-start gap-2">
                <span className="text-red-500 mt-0.5 text-sm">×</span>
                <span className="text-muted-foreground text-xs">
                  <strong>Local clusters:</strong> Resource-heavy, doesn't match
                  production environment
                </span>
              </div>
              <div className="flex items-start gap-2">
                <span className="text-red-500 mt-0.5 text-sm">×</span>
                <span className="text-muted-foreground text-xs">
                  <strong>Deploy to dev cluster:</strong> Slow feedback loop,
                  complex CI/CD setup
                </span>
              </div>
              <div className="flex items-start gap-2">
                <span className="text-green-500 mt-0.5 text-sm">✓</span>
                <span className="text-foreground text-xs">
                  <strong>mirrord:</strong> Run locally with full cloud context
                  in seconds
                </span>
              </div>
            </div>
          </div>
        </CardContent>
      </Card>
    </div>
  ),
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
            mirrord runs in two places - in the memory of your local process
            (mirrord-layer), and as a pod in your cloud environment
            (mirrord-agent).
          </p>

          <div className="bg-muted/30 rounded-lg p-4 mb-4">
            <h5 className="font-semibold mb-3 text-accent-foreground text-sm">
              mirrord - Basic Architecture
            </h5>
            <p className="text-muted-foreground mb-3 text-xs">
              When you start your local process with mirrord, it creates a pod
              in your cloud environment, which listens in on the pod you've
              passed as an argument. mirrord-layer then does the following:
            </p>

            <div className="ml-3 space-y-2">
              <p className="font-medium text-foreground text-xs">
                Override the process' syscalls to:
              </p>
              <ul className="space-y-1 ml-3 text-muted-foreground text-xs">
                <li>
                  • Listen to incoming traffic from the agent, instead of local
                  sockets
                </li>
                <li>
                  • Intercept outgoing traffic and send it out from the remote
                  pod, instead of locally
                </li>
                <li>• Read and write files to the remote file system</li>
                <li>
                  • Merge the process' environment variables with those of the
                  remote pod
                </li>
              </ul>
            </div>
          </div>

          <div className="bg-background border border-border rounded-lg p-2">
            <img
              src={mirroredArchitecture}
              alt="mirrord Architecture Diagram"
              className="w-full max-w-full mx-auto rounded"
            />
          </div>
        </CardContent>
      </Card>
    </div>
  ),
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
            <h5 className="font-semibold mb-3 text-foreground text-sm">
              Process Overview:
            </h5>
            <ol className="space-y-2 text-muted-foreground list-decimal ml-4 mb-3 text-xs">
              <li>Remote traffic is stolen selectively</li>
              <li>
                Remote resources such as databases and other services are used.
              </li>
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
  ),
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
              Your local service receives a copy of the incoming traffic sent to
              the targeted remote service. The response will come from the
              remote service and your local service will handle the same request
              but it's response will be discarded. By default, you will connect
              to the remote dependencies so you can have side effects (i.e
              writing to same database)
            </p>
            <p className="font-medium text-primary text-xs">
              Best for: Debugging existing traffic, understanding how APIs
              behave without deploying/changing logic in the environment.
            </p>
          </div>
        </CardContent>
      </Card>
    </div>
  ),
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
              Replace mode completely substitutes the remote service's traffic
              and queues. Useful when Queue Splitting isn't available and you
              want to work against queues.
            </p>
          </div>
        </CardContent>
      </Card>
    </div>
  ),
};

const learningStep6: WizardStep = {
  id: "wizard-6-feedback",
  title: "Development Feedback Loop",
  content: (
    <div className="space-y-4 max-w-3xl mx-auto">
      <Card className="bg-gradient-card border-border/50 shadow-glow">
        <CardHeader className="pb-3">
          <CardTitle className="text-lg">
            Fast Development Feedback Loop
          </CardTitle>
        </CardHeader>
        <CardContent className="pt-0">
          <p className="text-foreground text-sm leading-relaxed mb-4 text-center">
            mirrord creates a fast feedback loop that dramatically speeds up
            cloud-native development by eliminating the need for deployments
            during development.
          </p>

          <div className="bg-background border border-border rounded-lg p-2 mb-4">
            <img
              src="/lovable-uploads/0514df82-cea8-46c8-bfb5-05db3e6778e2.png"
              alt="Development Feedback Loop with mirrord"
              className="w-full max-w-full mx-auto rounded"
            />
          </div>
        </CardContent>
      </Card>
    </div>
  ),
};

const LearningSteps: WizardStep[] = [
  learningStep1,
  learningStep2,
  learningStep3,
  learningStep4,
  learningStep5,
  learningStep6,
];

export default LearningSteps;
