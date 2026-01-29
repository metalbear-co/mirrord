import { useContext } from "react";
import { Copy, Filter, Repeat, Check } from "lucide-react";
import { Badge } from "@metalbear/ui";
import { ConfigDataContext } from "../UserDataContext";
import {
  readBoilerplateType,
  updateConfigMode,
  updateConfigCopyTarget,
} from "../JsonUtils";

type BoilerplateType = "mirror" | "steal" | "replace";

interface ModeOption {
  id: BoilerplateType;
  icon: React.ReactNode;
  title: string;
  description: string;
  features: string[];
}

const modeOptions: ModeOption[] = [
  {
    id: "mirror",
    icon: <Copy className="h-5 w-5" />,
    title: "Mirror Mode",
    description:
      "Copy incoming traffic to your local environment without affecting the remote service. Useful when you want to observe traffic while the remote continues serving.",
    features: ["mirror mode", "non-disruptive"],
  },
  {
    id: "steal",
    icon: <Filter className="h-5 w-5" />,
    title: "Filtering Mode",
    description:
      "Selectively steal traffic based on HTTP headers or paths. Route specific requests to your local environment while others go to remote.",
    features: ["steal mode", "selective traffic"],
  },
  {
    id: "replace",
    icon: <Repeat className="h-5 w-5" />,
    title: "Replace Mode",
    description:
      "Completely replace the remote service with your local environment. All traffic goes to your local process.",
    features: ["steal mode", "copy target", "scale down"],
  },
];

const BoilerplateStep = () => {
  const { config, setConfig } = useContext(ConfigDataContext)!;
  const selectedMode = readBoilerplateType(config);

  const handleModeSelect = (mode: BoilerplateType) => {
    let newConfig = config;

    switch (mode) {
      case "mirror":
        newConfig = updateConfigMode("mirror", config);
        newConfig = updateConfigCopyTarget(false, false, newConfig);
        break;
      case "steal":
        newConfig = updateConfigMode("steal", config);
        newConfig = updateConfigCopyTarget(false, false, newConfig);
        break;
      case "replace":
        newConfig = updateConfigMode("steal", config);
        newConfig = updateConfigCopyTarget(true, true, newConfig);
        break;
    }

    setConfig(newConfig);
  };

  return (
    <div className="space-y-4">
      <div className="text-center mb-6">
        <h3 className="text-base font-medium text-[var(--foreground)] mb-2">
          How do you want to interact with remote traffic?
        </h3>
        <p className="text-sm text-[var(--muted-foreground)]">
          Choose a mode that fits your development workflow
        </p>
      </div>

      <div className="space-y-3">
        {modeOptions.map((option) => (
          <button
            key={option.id}
            onClick={() => handleModeSelect(option.id)}
            className={`
              w-full p-4 rounded-lg border text-left transition-all duration-200
              ${
                selectedMode === option.id
                  ? "border-primary bg-primary/5 ring-2 ring-primary/20"
                  : "border-[var(--border)] hover:border-[var(--muted-foreground)]/30 hover:bg-[var(--muted)]/30"
              }
            `}
          >
            <div className="flex items-start gap-4">
              <div
                className={`
                  w-10 h-10 rounded-lg flex items-center justify-center flex-shrink-0
                  ${
                    selectedMode === option.id
                      ? "bg-primary text-white"
                      : "bg-[var(--muted)] text-[var(--muted-foreground)]"
                  }
                `}
              >
                {option.icon}
              </div>
              <div className="flex-grow min-w-0">
                <div className="flex items-center gap-2 mb-1 flex-wrap">
                  <span className="font-medium text-[var(--foreground)]">
                    {option.title}
                  </span>
                  {option.features.map((feature) => (
                    <Badge
                      key={feature}
                      variant="outline"
                      className="text-xs font-normal"
                    >
                      {feature}
                    </Badge>
                  ))}
                </div>
                <p className="text-sm text-[var(--muted-foreground)]">
                  {option.description}
                </p>
              </div>
              {selectedMode === option.id && (
                <Check className="h-5 w-5 text-primary flex-shrink-0" />
              )}
            </div>
          </button>
        ))}
      </div>
    </div>
  );
};

export default BoilerplateStep;
