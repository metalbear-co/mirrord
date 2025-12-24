import { useState, type ReactNode } from "react";
import { ArrowRight } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader } from "../ui/card";
import { Button } from "../ui/button";
import AdminWizard, { type AdminWizardStep } from "./AdminWizard";

interface AdminPanelProps {
  title: ReactNode;
  content: string;
  buttonText: string;
  buttonColor: "purple" | "gray";
  steps: AdminWizardStep[];
}

const AdminPanel = ({
  title,
  content,
  buttonText,
  buttonColor,
  steps,
}: AdminPanelProps) => {
  const [showWizard, setShowWizard] = useState(false);

  const cardButton =
    buttonColor === "purple" ? (
      <Button
        onClick={() => setShowWizard(true)}
        className="w-full bg-gradient-primary hover:shadow-glow"
      >
        {buttonText}
        <ArrowRight className="h-4 w-4 ml-2" />
      </Button>
    ) : (
      <Button
        variant="outline"
        onClick={() => setShowWizard(true)}
        className="w-full"
      >
        {buttonText}
        <ArrowRight className="h-4 w-4 ml-2" />
      </Button>
    );

  return (
    <div className="grid gap-6 sm:gap-8 max-w-4xl mx-auto">
      <AdminWizard
        steps={steps}
        isOpen={showWizard}
        onClose={() => setShowWizard(false)}
      />
      <Card className="bg-gradient-card border-border/50 hover:shadow-glow transition-all duration-300">
        <CardHeader>
          {title}
          <CardDescription>{content}</CardDescription>
        </CardHeader>
        <CardContent>{cardButton}</CardContent>
      </Card>
    </div>
  );
};

export default AdminPanel;
