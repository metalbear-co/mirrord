import { ArrowRight, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";

interface PortMapping {
  remote: string;
  local: string;
}

interface PortConfigProps {
  ports: PortMapping[];
  mode: "steal" | "mirror" | "replace";
  onPortsChange: (ports: PortMapping[]) => void;
}

const availablePorts = ["8080", "3000", "5432", "9000", "4000", "6379", "5672", "3306"];

export function PortConfig({ ports, mode, onPortsChange }: PortConfigProps) {
  const togglePort = (port: string) => {
    const isSelected = ports.some(p => p.remote === port);
    
    if (isSelected) {
      onPortsChange(ports.filter(p => p.remote !== port));
    } else {
      onPortsChange([...ports, { remote: port, local: port }]);
    }
  };

  const updatePortMapping = (index: number, updates: Partial<PortMapping>) => {
    const newPorts = [...ports];
    newPorts[index] = { ...newPorts[index], ...updates };
    onPortsChange(newPorts);
  };

  const removePort = (remotePort: string) => {
    onPortsChange(ports.filter(p => p.remote !== remotePort));
  };

  return (
    <div className="space-y-4">
      <div>
        <h4 className="text-base font-semibold">Detected Ports</h4>
        <p className="text-sm text-muted-foreground mb-3">
          Click on a port to {mode === "steal" ? "steal traffic from" : "mirror traffic from"}
        </p>
        <div className="flex flex-wrap gap-2">
          {availablePorts.map(port => {
            const isSelected = ports.some(p => p.remote === port);
            
            return (
              <Button
                key={port}
                variant={isSelected ? "default" : "outline"}
                size="sm"
                className={`rounded-full px-4 py-2 font-mono transition-all ${
                  isSelected 
                    ? 'bg-primary text-primary-foreground hover:bg-primary/90' 
                    : 'hover:bg-primary/10'
                }`}
                onClick={() => togglePort(port)}
              >
                {port}
              </Button>
            );
          })}
        </div>
      </div>

      {ports.length > 0 && (
        <div className="space-y-3">
          <h4 className="text-base font-semibold">Selected Ports</h4>
          <p className="text-sm text-muted-foreground mb-3">Only map ports that run on different ports locally.</p>
          <div className="space-y-3">
            {ports.map((portConfig, index) => (
              <div key={portConfig.remote} className="border rounded-lg p-4 space-y-3">
                <div className="flex items-center justify-between">
                  <Badge variant="outline" className="font-mono">{portConfig.remote}</Badge>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-8 w-8 p-0 text-red-500 hover:text-red-600 hover:bg-red-50"
                    onClick={() => removePort(portConfig.remote)}
                  >
                    <Trash2 className="h-4 w-4" />
                  </Button>
                </div>
                
                {mode === "steal" && (
                  <>
                    <div className="flex items-center space-x-2">
                      <Checkbox 
                        id={`local-port-${portConfig.remote}`}
                        checked={portConfig.local !== portConfig.remote}
                        onCheckedChange={(checked) => {
                          updatePortMapping(index, {
                            local: checked ? "" : portConfig.remote
                          });
                        }}
                      />
                      <Label htmlFor={`local-port-${portConfig.remote}`} className="text-sm">
                        Local port is different than remote
                      </Label>
                    </div>
                    
                    {portConfig.local !== portConfig.remote && (
                      <div className="flex items-center gap-3">
                        <Input
                          className="font-mono flex-1"
                          placeholder="Local port"
                          value={portConfig.local}
                          onChange={(e) => updatePortMapping(index, { 
                            local: e.target.value || portConfig.remote 
                          })}
                        />
                        
                        <ArrowRight className="h-4 w-4 text-muted-foreground flex-shrink-0" />
                        
                        <Input
                          className="font-mono flex-1"
                          value={portConfig.remote}
                          readOnly
                        />
                      </div>
                    )}
                  </>
                )}
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}