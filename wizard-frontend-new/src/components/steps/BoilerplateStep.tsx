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
  recommended?: boolean;
}

const modeOptions: ModeOption[] = [
  {
    id: "mirror",
    icon: <Copy className="h-5 w-5" />,
    title: "Mirror Mode",
    description:
      "Copy incoming traffic to your local environment without affecting the remote service. Perfect for debugging production issues safely.",
    features: ["Non-disruptive", "Safe for production"],
    recommended: true,
  },
  {
    id: "steal",
    icon: <Filter className="h-5 w-5" />,
    title: "Filtering Mode",
    description:
      "Selectively intercept traffic based on HTTP headers or paths. Route specific requests to your local environment while others go to remote.",
    features: ["Selective traffic", "Header-based routing"],
  },
  {
    id: "replace",
    icon: <Repeat className="h-5 w-5" />,
    title: "Replace Mode",
    description:
      "Completely replace the remote service with your local environment. All traffic is routed to your local process.",
    features: ["Full replacement", "Scale down remote"],
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
    <div className="space-y-6">
      <div className="text-center mb-8">
        <h3 className="text-lg font-medium text-[var(--foreground)] mb-2">
          How do you want to interact with remote traffic?
        </h3>
        <p className="text-sm text-[var(--muted-foreground)]">
          Choose a mode that fits your development workflow
        </p>
      </div>

      <div className="space-y-3">
        {modeOptions.map((option, index) => {
          const isSelected = selectedMode === option.id;

          return (
            <button
              key={option.id}
              onClick={() => handleModeSelect(option.id)}
              className={`
                w-full p-5 rounded-xl border text-left transition-all duration-200 animate-fade-in
                ${isSelected
                  ? "border-primary bg-primary/5 shadow-brand"
                  : "border-[var(--border)] hover:border-primary/30 hover:bg-[var(--muted)]/30 hover:shadow-sm"
                }
              `}
              style={{ animationDelay: `${index * 50}ms` }}
            >
              <div className="flex items-start gap-4">
                <div
                  className={`
                    w-12 h-12 rounded-xl flex items-center justify-center flex-shrink-0 transition-colors duration-200
                    ${isSelected
                      ? "bg-primary text-white shadow-brand"
                      : "bg-[var(--muted)] text-[var(--muted-foreground)]"
                    }
                  `}
                >
                  {option.icon}
                </div>
                <div className="flex-grow min-w-0">
                  <div className="flex items-center gap-2 mb-2 flex-wrap">
                    <span className={`font-semibold ${isSelected ? "text-primary" : "text-[var(--foreground)]"}`}>
                      {option.title}
                    </span>
                    {option.recommended && (
                      <Badge variant="secondary" className="text-xs bg-secondary/20 text-secondary-foreground border-secondary/30">
                        Recommended
                      </Badge>
                    )}
                  </div>
                  <p className="text-sm text-[var(--muted-foreground)] mb-3 leading-relaxed">
                    {option.description}
                  </p>
                  <div className="flex flex-wrap gap-2">
                    {option.features.map((feature) => (
                      <Badge
                        key={feature}
                        variant="outline"
                        className={`text-xs font-normal ${isSelected ? "border-primary/30 text-primary" : ""}`}
                      >
                        {feature}
                      </Badge>
                    ))}
                  </div>
                </div>
                <div className={`flex-shrink-0 transition-all duration-200 ${isSelected ? "opacity-100 scale-100" : "opacity-0 scale-75"}`}>
                  <div className="w-6 h-6 rounded-full bg-primary flex items-center justify-center">
                    <Check className="h-4 w-4 text-white" />
                  </div>
                </div>
              </div>
            </button>
          );
        })}
      </div>
    </div>
  );
};

export default BoilerplateStep;
