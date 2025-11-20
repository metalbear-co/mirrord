import React from "react";
import { Card, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Check } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { Copy, Filter, Repeat } from "lucide-react";
import { WizardStep } from "../Wizard";
import { useContext, useState } from "react";
import { ConfigDataContext } from "../UserDataContext";
import { updateConfigCopyTarget, updateConfigMode } from "../JsonUtils";

export interface BoilerplateCardProps {
  id: string;
  title: string;
  description: string;
  features: string[];
  icon: LucideIcon;
  color: string;
  selected: boolean;
  onClick: () => void;
}

export const BoilerplateCard: React.FC<BoilerplateCardProps> = ({
  id,
  title,
  description,
  features,
  icon: Icon,
  color,
  selected = false,
  onClick,
}) => {
  return (
    <Card
      key={id}
      className={`cursor-pointer transition-all hover:shadow-md ${
        selected ? "ring-2 ring-primary border-primary/50" : ""
      }`}
      onClick={onClick}
    >
      <CardHeader className="pb-3 pt-3">
        <div className="flex items-start gap-3">
          <div
            className={`p-2 rounded-lg ${
              selected ? "bg-primary text-primary-foreground" : "bg-muted"
            }`}
          >
            <Icon className="h-4 w-4" />
          </div>
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 flex-wrap mb-1">
              <CardTitle className="text-base">{title}</CardTitle>
              <div className="flex flex-wrap gap-1">
                {features.map((feature) => (
                  <Badge key={feature} variant="secondary" className="text-xs">
                    {feature}
                  </Badge>
                ))}
              </div>
            </div>
            <p className="text-xs text-muted-foreground">{description}</p>
          </div>
          {selected && <Check className="h-4 w-4 text-primary flex-shrink-0" />}
        </div>
      </CardHeader>
    </Card>
  );
};

const BoilerplateStep: () => WizardStep = () => {
  const boilerplateConfigs: BoilerplateCardProps[] = [
    {
      id: "steal",
      title: "Filtering mode",
      description:
        "Suitable for scenarios where you want to see how your changes impact remote environment while reducing the impact radius",
      features: ["steal mode", "selective traffic"],
      icon: Filter,
      color: "text-purple-500",
      selected: false,
      onClick: () => {},
    },
    {
      id: "mirror",
      title: "Mirror mode",
      description:
        "This is useful when you want the remote target to serve requests and you're okay with one request being handled twice",
      features: ["mirror mode"],
      icon: Copy,
      color: "text-blue-500",
      selected: false,
      onClick: () => {},
    },
    {
      id: "replace",
      title: "Replace mode",
      description:
        "Suitable for scenarios where you have your own namespace/cluster and you're okay with replacing the remote service entirely. Note: Cannot replace pods, only other entities.",
      features: ["steal mode", "copy target", "scale down"],
      icon: Repeat,
      color: "text-orange-500",
      selected: false,
      onClick: () => {},
    },
  ];

  const { config, setConfig } = useContext(ConfigDataContext);
  const [selectedBoilerplate, setSelectedBoilerplate] = useState<string>("");
  const handleBoilerplateSelect = (boilerplateId: string) => {
    setSelectedBoilerplate(boilerplateId);
    if (boilerplateId === "replace") {
      const newConfig = updateConfigCopyTarget(true, true, config);
      const finalConfig = updateConfigMode("steal", newConfig);
      setConfig(finalConfig);
    } else if (boilerplateId === "mirror") {
      const newConfig = updateConfigCopyTarget(false, false, config);
      const finalConfig = updateConfigMode("mirror", newConfig);
      setConfig(finalConfig);
    } else if (boilerplateId === "steal") {
      const newConfig = updateConfigCopyTarget(false, false, config);
      const finalConfig = updateConfigMode("steal", newConfig);
      setConfig(finalConfig);
    }
  };

  return {
    id: "boilerplate",
    title: "mirrord configuration",
    content: (
      <div className="space-y-6">
        <div className="flex flex-col gap-2">
          {boilerplateConfigs.map((boilerplate) => (
            <BoilerplateCard
              key={boilerplate.id}
              {...boilerplate}
              selected={selectedBoilerplate === boilerplate.id}
              onClick={() => handleBoilerplateSelect(boilerplate.id)}
            />
          ))}
        </div>
      </div>
    ),
    allowProgress: selectedBoilerplate?.length > 0,
  };
};

export default BoilerplateStep;
