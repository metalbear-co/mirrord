import { useState, type ReactNode } from "react";
import { type WizardStep, Wizard } from "./Wizard";
import { Card, CardContent, CardDescription, CardHeader } from "./ui/card";
import { ArrowRight } from "lucide-react";
import { Button } from "./ui/button";

interface PanelProps {
  title: ReactNode;
  content: string;
  buttonText: string;
  buttonColor: "purple" | "gray";
  steps: WizardStep[];
}

const Panel = ({
  title,
  content,
  buttonText,
  buttonColor,
  steps,
}: PanelProps) => {
  const [showWizard, setShowWizard] = useState(false);

  const handleWizardOpen = () => {
    setShowWizard(true);
  };

  const cardButton = () => {
    if (buttonColor == "purple") {
      return (
        <Button
          onClick={() => handleWizardOpen()}
          className="w-full bg-gradient-primary hover:shadow-glow"
        >
          {buttonText}
          <ArrowRight className="h-4 w-4 ml-2" />
        </Button>
      );
    } else {
      return (
        <Button
          variant="outline"
          onClick={() => handleWizardOpen()}
          className="w-full"
        >
          {buttonText}
          <ArrowRight className="h-4 w-4 ml-2" />
        </Button>
      );
    }
  };

  return (
    <div>
      <div className="grid gap-6 sm:gap-8 max-w-4xl mx-auto">
        <Wizard
          steps={steps}
          isOpen={showWizard}
          onClose={() => setShowWizard(false)}
          className="w-full"
        />
        <Card className="bg-gradient-card border-border/50 hover:shadow-glow transition-all duration-300">
          <CardHeader>
            {title}
            <CardDescription>{content}</CardDescription>
          </CardHeader>
          <CardContent>{cardButton()}</CardContent>
        </Card>
      </div>
    </div>
  );
};

export default Panel;
