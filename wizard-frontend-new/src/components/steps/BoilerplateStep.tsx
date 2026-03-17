import { useContext } from "react";
import { Copy, Filter, Repeat, Check, Shield, Zap } from "lucide-react";
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
  subtitle: string;
  description: string;
  features: { label: string; icon: React.ReactNode }[];
  recommended?: boolean;
  accentClass: string;
  selectedBg: string;
  iconBg: string;
  iconBgSelected: string;
}

const modeOptions: ModeOption[] = [
  {
    id: "mirror",
    icon: <Copy className="h-5 w-5" />,
    title: "Mirror",
    subtitle: "Observe without impact",
    description:
      "Copy incoming traffic to your local environment without affecting the remote service.",
    features: [
      { label: "Non-disruptive", icon: <Shield className="h-3 w-3" /> },
      { label: "Safe for production", icon: <Check className="h-3 w-3" /> },
    ],
    recommended: true,
    accentClass: "border-primary",
    selectedBg: "bg-primary/5",
    iconBg: "bg-primary/10 text-primary",
    iconBgSelected: "bg-primary text-white",
  },
  {
    id: "steal",
    icon: <Filter className="h-5 w-5" />,
    title: "Filter",
    subtitle: "Selective interception",
    description:
      "Intercept specific requests based on HTTP headers or paths while others pass through.",
    features: [
      { label: "Header-based routing", icon: <Zap className="h-3 w-3" /> },
      { label: "Selective traffic", icon: <Filter className="h-3 w-3" /> },
    ],
    accentClass: "border-secondary",
    selectedBg: "bg-secondary/5",
    iconBg: "bg-secondary/15 text-secondary-foreground",
    iconBgSelected: "bg-secondary text-secondary-foreground",
  },
  {
    id: "replace",
    icon: <Repeat className="h-5 w-5" />,
    title: "Replace",
    subtitle: "Full takeover",
    description:
      "Completely replace the remote service. All traffic is routed to your local process.",
    features: [
      { label: "Full replacement", icon: <Repeat className="h-3 w-3" /> },
      { label: "Scale down remote", icon: <Zap className="h-3 w-3" /> },
    ],
    accentClass: "border-destructive",
    selectedBg: "bg-destructive/5",
    iconBg: "bg-destructive/10 text-destructive",
    iconBgSelected: "bg-destructive text-white",
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
      <div className="text-center mb-6">
        <h3 className="text-lg font-semibold text-[var(--foreground)] mb-1">
          How should mirrord handle traffic?
        </h3>
        <p className="text-sm text-[var(--muted-foreground)]">
          Pick a mode that matches your workflow
        </p>
      </div>

      <div className="space-y-3">
        {modeOptions.map((option) => {
          const isSelected = selectedMode === option.id;

          return (
            <button
              key={option.id}
              onClick={() => handleModeSelect(option.id)}
              className={`
                w-full p-5 rounded-xl border-2 text-left transition-all duration-200 relative
                ${
                  isSelected
                    ? `${option.accentClass} ${option.selectedBg} shadow-md`
                    : "border-[var(--border)] hover:border-[var(--border)] hover:shadow-md hover:bg-[var(--muted)]/30 active:scale-[0.995]"
                }
              `}
            >
              {option.recommended && (
                <div className="absolute -top-2.5 right-4">
                  <Badge className="bg-primary text-white text-[10px] font-semibold px-2.5 py-0.5 shadow-brand">
                    Recommended
                  </Badge>
                </div>
              )}
              <div className="flex items-start gap-4">
                <div
                  className={`
                    w-11 h-11 rounded-xl flex items-center justify-center flex-shrink-0 transition-all duration-200
                    ${isSelected ? option.iconBgSelected : option.iconBg}
                  `}
                >
                  {option.icon}
                </div>
                <div className="flex-grow min-w-0">
                  <div className="flex items-baseline gap-2 mb-1">
                    <span className="font-semibold text-[var(--foreground)]">
                      {option.title}
                    </span>
                    <span className="text-xs text-[var(--muted-foreground)]">
                      {option.subtitle}
                    </span>
                  </div>
                  <p className="text-sm text-[var(--muted-foreground)] mb-3 leading-relaxed">
                    {option.description}
                  </p>
                  <div className="flex flex-wrap gap-2">
                    {option.features.map((feature) => (
                      <span
                        key={feature.label}
                        className={`inline-flex items-center gap-1.5 text-xs px-2.5 py-1 rounded-full border transition-colors ${
                          isSelected
                            ? "border-current/20 text-[var(--foreground)] bg-white/50 dark:bg-white/5"
                            : "border-[var(--border)] text-[var(--muted-foreground)]"
                        }`}
                      >
                        {feature.icon}
                        {feature.label}
                      </span>
                    ))}
                  </div>
                </div>
                <div
                  className={`flex-shrink-0 transition-all duration-200 ${isSelected ? "opacity-100 scale-100" : "opacity-0 scale-75"}`}
                >
                  <div className="w-6 h-6 rounded-full bg-primary flex items-center justify-center shadow-sm">
                    <Check className="h-3.5 w-3.5 text-white" />
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
