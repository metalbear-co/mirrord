import { Network, Plus, Trash2 } from "lucide-react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Separator } from "@/components/ui/separator";
import { PortConfig } from "./PortConfig";

interface HttpFilter {
  type: "header" | "method" | "content" | "path";
  value: string;
  matchType?: "exact" | "regex";
}

interface NetworkConfigData {
  incoming: {
    enabled: boolean;
    mode: "steal" | "mirror";
    httpFilter: HttpFilter[];
    filterOperator: "AND" | "OR";
    ports: Array<{
      remote: string;
      local: string;
    }>;
  };
}

interface NetworkConfigProps {
  selectedBoilerplate: string;
  networkConfig: NetworkConfigData;
  onNetworkChange: (updates: Partial<NetworkConfigData>) => void;
}

export function NetworkConfig({ 
  selectedBoilerplate, 
  networkConfig, 
  onNetworkChange 
}: NetworkConfigProps) {
  const addHttpFilter = (type: "header" | "path") => {
    const newFilter: HttpFilter = {
      type,
      value: "",
      ...(type === "header" ? { matchType: "exact" as const } : {})
    };
    
    onNetworkChange({
      incoming: {
        ...networkConfig.incoming,
        httpFilter: [...networkConfig.incoming.httpFilter, newFilter]
      }
    });
  };

  const updateHttpFilter = (index: number, updates: Partial<HttpFilter>) => {
    const newFilters = [...networkConfig.incoming.httpFilter];
    newFilters[index] = { ...newFilters[index], ...updates };
    
    onNetworkChange({
      incoming: {
        ...networkConfig.incoming,
        httpFilter: newFilters
      }
    });
  };

  const removeHttpFilter = (index: number) => {
    const newFilters = networkConfig.incoming.httpFilter.filter((_, i) => i !== index);
    
    onNetworkChange({
      incoming: {
        ...networkConfig.incoming,
        httpFilter: newFilters
      }
    });
  };

  const handlePortsChange = (ports: Array<{ remote: string; local: string }>) => {
    onNetworkChange({
      incoming: {
        ...networkConfig.incoming,
        ports
      }
    });
  };

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="flex items-center gap-2 text-base">
          <Network className="h-4 w-4" />
          Network Configuration
        </CardTitle>
      </CardHeader>
      <CardContent>
        <div className="space-y-6">
          {selectedBoilerplate === "steal" && (
            <div className="space-y-4">
              <div>
                <h3 className="text-base font-semibold mb-1">Traffic Filtering</h3>
                <p className="text-xs text-muted-foreground mb-3">
                  Steal a subset of traffic by specifying filters on HTTP headers or paths
                </p>
              </div>

              {/* Header Filters */}
              <div className="space-y-2">
                <div className="flex items-center justify-between">
                  <Label className="font-medium">Header Filters</Label>
                  <Button 
                    type="button" 
                    variant="outline" 
                    size="sm" 
                    onClick={() => addHttpFilter("header")}
                  >
                    <Plus className="h-4 w-4 mr-1" />
                    Add Header
                  </Button>
                </div>
                
                {networkConfig.incoming.httpFilter.filter(f => f.type === "header").map((filter, index) => {
                  const globalIndex = networkConfig.incoming.httpFilter.findIndex(f => f === filter);
                  return (
                    <div key={globalIndex} className="flex items-center gap-2">
                      <Input 
                        placeholder="e.g., x-mirrord-test: true" 
                        value={filter.value} 
                        onChange={(e) => updateHttpFilter(globalIndex, { value: e.target.value })}
                        className="flex-1" 
                      />
                      <Select 
                        value={filter.matchType || "exact"} 
                        onValueChange={(value: "exact" | "regex") => 
                          updateHttpFilter(globalIndex, { matchType: value })
                        }
                      >
                        <SelectTrigger className="w-20">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          <SelectItem value="exact">Exact</SelectItem>
                          <SelectItem value="regex">Regex</SelectItem>
                        </SelectContent>
                      </Select>
                      <Button 
                        type="button" 
                        variant="outline" 
                        size="sm" 
                        onClick={() => removeHttpFilter(globalIndex)}
                      >
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    </div>
                  );
                })}
              </div>

              {/* Path Filters */}
              <div className="space-y-2">
                <div className="flex items-center justify-between">
                  <Label className="font-medium">Path Filters</Label>
                  <Button 
                    type="button" 
                    variant="outline" 
                    size="sm" 
                    onClick={() => addHttpFilter("path")}
                  >
                    <Plus className="h-4 w-4 mr-1" />
                    Add Path
                  </Button>
                </div>
                
                {networkConfig.incoming.httpFilter.filter(f => f.type === "path").map((filter, index) => {
                  const globalIndex = networkConfig.incoming.httpFilter.findIndex(f => f === filter);
                  return (
                    <div key={globalIndex} className="flex items-center gap-2">
                      <Input 
                        placeholder="e.g., /api/v1/test" 
                        value={filter.value} 
                        onChange={(e) => updateHttpFilter(globalIndex, { value: e.target.value })}
                      />
                      <Button 
                        type="button" 
                        variant="outline" 
                        size="sm" 
                        onClick={() => removeHttpFilter(globalIndex)}
                      >
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    </div>
                  );
                })}
              </div>

              {/* Filter Logic Selection */}
              {networkConfig.incoming.httpFilter.length > 1 && (
                <>
                  <Separator />
                  <div className="space-y-2">
                    <Label className="font-medium">Filter Logic</Label>
                    <RadioGroup 
                      value={networkConfig.incoming.filterOperator} 
                      onValueChange={(value: "AND" | "OR") => 
                        onNetworkChange({
                          incoming: {
                            ...networkConfig.incoming,
                            filterOperator: value
                          }
                        })
                      }
                    >
                      <div className="flex items-center space-x-2">
                        <RadioGroupItem value="AND" id="and" />
                        <Label htmlFor="and" className="text-sm">
                          <strong>All</strong> - Match all specified filters
                        </Label>
                      </div>
                      <div className="flex items-center space-x-2">
                        <RadioGroupItem value="OR" id="or" />
                        <Label htmlFor="or" className="text-sm">
                          <strong>Any</strong> - Match any specified filter
                        </Label>
                      </div>
                    </RadioGroup>
                  </div>
                </>
              )}
            </div>
          )}

          {/* Port Configuration */}
          <div className="space-y-4">
            <div>
              <h3 className="text-base font-semibold mb-1">Port Configuration</h3>
            </div>
            
            <PortConfig
              ports={networkConfig.incoming.ports}
              mode={networkConfig.incoming.mode}
              onPortsChange={handlePortsChange}
            />
          </div>
        </div>
      </CardContent>
    </Card>
  );
}