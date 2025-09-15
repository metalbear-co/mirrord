import { Button } from "@/components/ui/button";
import {
    ChevronRight,
    ChevronLeft,
    BookOpen,
    SkipForward,
    Play,
} from "lucide-react";
import type { ConfigData } from "@/types/config";

type OnboardingStep =
    | "intro"
    | "explanation"
    | "boilerplate"
    | "config";

interface WizardFooterProps {
    isReturning: boolean;
    onboardingStep: OnboardingStep;
    currentTab: string;
    configTarget: string;
    jsonError: string;
    editableJson: string;
    setOnboardingStep: (step: OnboardingStep) => void;
    setCurrentTab: (tab: string) => void;
    generateConfigJson: (config: ConfigData) => string;
    updateConfigFromJson: (
        jsonString: string,
        setConfig: React.Dispatch<React.SetStateAction<ConfigData>>,
        setJsonError: (error: string) => void
    ) => void;
    setConfig: React.Dispatch<React.SetStateAction<ConfigData>>;
    setJsonError: (error: string) => void;
    config: ConfigData;
    onSave: (config: ConfigData) => void;
    onClose: () => void;
}

export function WizardFooter({
    isReturning,
    onboardingStep,
    currentTab,
    configTarget,
    jsonError,
    editableJson,
    setOnboardingStep,
    setCurrentTab,
    generateConfigJson,
    updateConfigFromJson,
    setConfig,
    setJsonError,
    config,
    onSave,
    onClose,
}: WizardFooterProps) {
    const IntroStepFooter = (
        <div className="flex gap-3 justify-center items-center">
            <Button
                variant="outline"
                onClick={() => setOnboardingStep("explanation")}
            >
                <BookOpen className="h-4 w-4 mr-2" />
                How mirrord Works
            </Button>
            <Button onClick={() => setOnboardingStep("boilerplate")}>
                <SkipForward className="h-4 w-4 mr-2" />
                Skip to Configuration
            </Button>
        </div>
    );

    const ExplStepFooter = (
        <div className="flex gap-3 justify-center items-center">
            <Button
                variant="outline"
                onClick={() => setOnboardingStep("intro")}
            >
                <ChevronLeft className="h-4 w-4 mr-2" />
                Back
            </Button>
            <Button onClick={() => setOnboardingStep("boilerplate")}>
                Continue
                <ChevronRight className="h-4 w-4 ml-2" />
            </Button>
        </div>
    );

    return (
        <div className="border-t bg-background p-4">
            {/* First-time user flow navigation */}
            {!isReturning && (
                <>
                    {onboardingStep === "intro" && IntroStepFooter}
                    {onboardingStep === "explanation" && ExplStepFooter}
                </>
            )}

            {/* Configuration tabs navigation */}
            {(isReturning || onboardingStep === "config") && (
                <>
                    {currentTab === "target" && (
                        <div className="flex justify-between">
                            {(!isReturning || onboardingStep === "config") && (
                                <Button
                                    variant="outline"
                                    onClick={() => setOnboardingStep("boilerplate")}
                                >
                                    <ChevronLeft className="h-4 w-4 mr-2" />
                                    Back
                                </Button>
                            )}
                            <Button
                                onClick={() => setCurrentTab("network")}
                                disabled={!configTarget}
                                className={isReturning ? "ml-auto" : ""}
                            >
                                Next
                                <ChevronRight className="h-4 w-4 ml-2" />
                            </Button>
                        </div>
                    )}

                    {currentTab === "network" && (
                        <div className="flex justify-between">
                            <Button
                                variant="outline"
                                onClick={() => setCurrentTab("target")}
                            >
                                <ChevronLeft className="h-4 w-4 mr-2" />
                                Back
                            </Button>
                            <div className="flex gap-2">
                                <Button
                                    variant="outline"
                                    onClick={() => setCurrentTab("export")}
                                >
                                    Skip
                                </Button>
                                <Button onClick={() => setCurrentTab("export")}>
                                    Next
                                    <ChevronRight className="h-4 w-4 ml-2" />
                                </Button>
                            </div>
                        </div>
                    )}

                    {currentTab === "export" && (
                        <div className="flex justify-between">
                            <Button
                                variant="outline"
                                onClick={() => setCurrentTab("network")}
                            >
                                <ChevronLeft className="h-4 w-4 mr-2" />
                                Back
                            </Button>
                            <div className="flex gap-2">
                                <Button
                                    onClick={() => {
                                        if (!jsonError && configTarget) {
                                            updateConfigFromJson(
                                                editableJson || generateConfigJson(config),
                                                setConfig,
                                                setJsonError
                                            );
                                            onSave({
                                                ...config,
                                                isActive: true,
                                            });
                                            onClose();
                                        }
                                    }}
                                    disabled={!!jsonError || !configTarget}
                                >
                                    <Play className="h-4 w-4 mr-2" />
                                    Save & Set Active
                                </Button>
                            </div>
                        </div>
                    )}
                </>
            )}
        </div>
    );
}
