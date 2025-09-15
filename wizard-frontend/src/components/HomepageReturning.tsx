import { Plus, BookOpen, ArrowRight, FileCode2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import Panel from "./Panel";

function onWizardOpen (mode: "create" | "overview"): void {
  // todo: open wizard with correct steps
  return;
}

export const HomepageReturning = () => {
  const isReturning = true;
  const titleCreateConfig = (
    <CardTitle className="flex items-center gap-2">
      <Plus className="h-5 w-5" /> Create Configuration
    </CardTitle>
  );
  const titleLearn = (
    <CardTitle className="flex items-center gap-2">
      <BookOpen className="h-5 w-5" /> Learn About mirrord
    </CardTitle>
  );

  return (
    <div className="min-h-screen w-full bg-background flex items-center justify-center p-6">
      <div className="max-w-4xl mx-auto">
        <div className="text-center mb-8 sm:mb-12">
          <h1 className="text-2xl sm:text-3xl font-bold mb-4 text-foreground">
            Welcome back! 👋
          </h1>
          <p className="text-muted-foreground text-sm sm:text-lg max-w-2xl mx-auto">
            Ready to create your next mirrord configuration or learn more about the platform?
          </p>
        </div>
        <div className="grid gap-6 sm:gap-8  md:grid-cols-2 max-w-4xl mx-auto">
          <Panel title={titleCreateConfig} content={"Use our wizard to create a working mirrord.json configuration file for your project"} buttonText={"Create New Config"} buttonColor={"purple"} steps={[]} isReturning={isReturning}/>
          <Panel title={titleLearn} content={"Explore how mirrord works and understand the different modes and configurations"} buttonText={"View Overview"} buttonColor={"gray"} steps={[]} isReturning={isReturning}/>
        </div>
      </div>
    </div>
  );
};
