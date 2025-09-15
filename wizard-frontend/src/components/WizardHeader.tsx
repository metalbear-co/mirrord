import { Badge } from "@/components/ui/badge";
import {
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { StepHeader } from "./StepHeader";
import type { BoilerplateCardProps } from "./BoilerplateCard";

type OnboardingStep = "intro" | "explanation" | "boilerplate" | "config";

interface WizardHeaderProps {
    isReturning: boolean;
    onboardingStep: OnboardingStep;
    selectedBoilerplate: string;
    boilerplateConfigs: BoilerplateCardProps[];
    currentTab: string;
    onTabChange: (value: string) => void;
    configTarget: string;
}

export function WizardHeader({
    isReturning,
    onboardingStep,
    selectedBoilerplate,
    boilerplateConfigs,
    currentTab,
    onTabChange,
    configTarget,
}: WizardHeaderProps) {
    const IntroStepHeader = (
        <StepHeader
            title="Welcome to mirrord Configuration"
            description="Let's create your first mirrord configuration to connect your local development environment with your Kubernetes cluster."
            titleSpecialFormat="text-2xl"
        />
    );

    const ExplStepHeader = (
        <StepHeader
            title="How mirrord Works"
            description="Understanding mirroring vs stealing and how they impact your development workflow"
        />
    );

    const BoilerplateStepHeader = (
        <StepHeader
            title="mirrord configuration"
            description="Select a configuration that matches your use case. You can customize it afterwards."
        />
    );

    const ConfigStepHeader = (
        <StepHeader
            title="Configuration Setup"
            description="Configure your mirrord settings using the tabs below"
            badge={
                !isReturning ? (
                    <Badge variant="secondary">
                        {boilerplateConfigs.find((b) => b.id === selectedBoilerplate)?.title}
                    </Badge>
                ) : undefined
            }
        />
    );

    const ConfigStepTabs = (<Tabs
        value={currentTab}
        onValueChange={onTabChange}
        className="w-full"
    >
        <TabsList className="grid w-full grid-cols-3 h-10">
            <TabsTrigger value="target" className="text-sm">
                Target
            </TabsTrigger>
            <TabsTrigger
                value="network"
                className="text-sm"
                disabled={!configTarget}
            >
                Network
            </TabsTrigger>
            <TabsTrigger
                value="export"
                className="text-sm"
                disabled={!configTarget}
            >
                Export
            </TabsTrigger>
        </TabsList>
    </Tabs>);

    return (
        <div className="bg-background p-4 flex-shrink-0">
            {/* Configuration interface header and tabs */}
            {(isReturning || onboardingStep === "config") && (
                <>
                    {ConfigStepHeader}
                    {ConfigStepTabs}
                </>
            )}

            {/* First-time user flow headers */}
            {!isReturning && (
                <>
                    {onboardingStep === "intro" && IntroStepHeader}
                    {onboardingStep === "explanation" && ExplStepHeader}
                    {onboardingStep === "boilerplate" && BoilerplateStepHeader}
                </>
            )}
        </div>
    );
}
