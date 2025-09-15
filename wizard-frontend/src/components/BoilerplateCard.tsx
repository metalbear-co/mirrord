 import React from "react";
import { Card, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Check } from "lucide-react";
import type { LucideIcon } from "lucide-react";

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
          {selected && (
            <Check className="h-4 w-4 text-primary flex-shrink-0" />
          )}
        </div>
      </CardHeader>
    </Card>
  );
};

export default BoilerplateCard;


