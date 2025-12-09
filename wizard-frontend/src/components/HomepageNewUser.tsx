import { Zap, BookOpen, ChevronLeft, ChevronRight, Server } from "lucide-react";
import {  CardTitle } from "@/components/ui/card";
import { WizardStep } from "@/components/Wizard";
import mirrordLogo from "@/assets/mirrord-logo.svg";
import Panel from "./Panel";
import BoilerplateStep from "./steps/BoilerplateStep";
import IntroStep from "./steps/IntroStep";
import ConfigStep from "./steps/ConfigStep";
import {LearningStepsNewUser} from "./steps/LearningSteps";

const HomepageNewUser = () => {
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

  const introStep = IntroStep();
  const boilerplateStep = BoilerplateStep();
  const configStep = ConfigStep();

  const configSteps: WizardStep[] = [boilerplateStep, configStep];
  const learnSteps: WizardStep[] = [introStep].concat(
    LearningStepsNewUser,
    configSteps
  );

  return (
    <div className="min-h-screen w-full bg-background flex items-center justify-center p-4">
      <div className="max-w-4xl mx-auto">
        <div className="text-center mb-8 sm:mb-12">
          <div className="flex items-center justify-center mb-6">
            <img
              src={mirrordLogo}
              alt="mirrord"
              className="size-40"
            />
          </div>

          <h1 className="text-2xl sm:text-3xl font-bold mb-4 text-foreground">
            Welcome to mirrord! 👋
          </h1>
          <p className="text-muted-foreground text-sm sm:text-lg max-w-2xl mx-auto">
            Run your local code in your Kubernetes cluster without the
            complexity of deployments. Let's get you started!
          </p>
        </div>
        <div className="grid gap-6 sm:gap-8  md:grid-cols-2 max-w-4xl mx-auto">
          <Panel
            title={titleLearn}
            content={
              "Understand how mirrord works and explore the overview before creating your first configuration"
            }
            buttonText={"Show Me How It Works"}
            buttonColor={"purple"}
            steps={learnSteps}
          />
          <Panel
            title={titleCreateConfig}
            content={
              "Jump directly to creating your mirrord.json configuration file"
            }
            buttonText={"Create Configuration Now"}
            buttonColor={"gray"}
            steps={configSteps}
          />
        </div>
      </div>
    </div>
  );
};

export default HomepageNewUser;
