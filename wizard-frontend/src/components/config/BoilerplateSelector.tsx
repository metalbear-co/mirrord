import { Check } from "lucide-react";
import { Card, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";

interface BoilerplateConfig {
  id: string;
  title: string;
  description: string;
  features: string[];
  icon: React.ComponentType<{ className?: string }>;
  color: string;
}

interface BoilerplateSelectorProps {
  boilerplates: BoilerplateConfig[];
  selectedBoilerplate: string;
  onSelect: (boilerplateId: string) => void;
}

export function BoilerplateSelector({
  boilerplates,
  selectedBoilerplate,
  onSelect
}: BoilerplateSelectorProps) {
  return (
    <div className="flex flex-col gap-3">
      {boilerplates.map(boilerplate => {
        const Icon = boilerplate.icon;
        const isSelected = selectedBoilerplate === boilerplate.id;
        
        return (
          <Card 
            key={boilerplate.id} 
            className={`cursor-pointer transition-all hover:shadow-md ${
              isSelected ? 'ring-2 ring-primary border-primary/50' : ''
            }`} 
            onClick={() => onSelect(boilerplate.id)}
          >
            <CardHeader className="pb-4 pt-4">
              <div className="flex items-start gap-2">
                <div className={`p-1.5 rounded-lg ${
                  isSelected ? 'bg-primary text-primary-foreground' : 'bg-muted'
                }`}>
                  <Icon className="h-4 w-4" />
                </div>
                <div className="flex-1">
                  <div className="flex items-center gap-2 flex-wrap">
                    <CardTitle className="text-base">{boilerplate.title}</CardTitle>
                    <div className="flex flex-wrap gap-1">
                      {boilerplate.features.map(feature => (
                        <Badge key={feature} variant="secondary" className="text-xs">
                          {feature}
                        </Badge>
                      ))}
                    </div>
                  </div>
                  <p className="text-xs text-muted-foreground mt-0.5">
                    {boilerplate.description}
                  </p>
                </div>
                {isSelected && <Check className="h-4 w-4 text-primary flex-shrink-0 mt-0.5" />}
              </div>
            </CardHeader>
          </Card>
        );
      })}
    </div>
  );
}