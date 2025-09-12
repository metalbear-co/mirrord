import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { HardDrive, Network, Settings2 } from "lucide-react";
import { TargetSelector } from "./TargetSelector";
import { NetworkConfig } from "./NetworkConfig";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Switch } from "@/components/ui/switch";
import { Label } from "@/components/ui/label";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";

interface Target {
  name: string;
  namespace: string;
  kind: string;
}

interface ConfigData {
  name: string;
  target: string;
  targetType: string;
  namespace: string;
  service?: string;
  fileSystem: {
    enabled: boolean;
    mode: "read" | "write" | "local";
    rules: Array<{
      mode: "read" | "write" | "local";
      filter: string;
    }>;
  };
  network: {
    incoming: {
      enabled: boolean;
      mode: "steal" | "mirror";
      httpFilter: Array<{
        type: "header" | "method" | "content" | "path";
        value: string;
        matchType?: "exact" | "regex";
      }>;
      filterOperator: "AND" | "OR";
      ports: Array<{
        remote: string;
        local: string;
      }>;
    };
    outgoing: {
      enabled: boolean;
      protocol: "tcp" | "udp" | "both";
      filter: string;
      filterTarget: "remote" | "local";
    };
    dns: {
      enabled: boolean;
      filter: string;
    };
  };
  environment: {
    enabled: boolean;
    include: string;
    exclude: string;
    override: string;
  };
  agent: {
    scaledown: boolean;
    copyTarget: boolean;
  };
  isActive: boolean;
}

interface ConfigTabsProps {
  config: ConfigData;
  selectedBoilerplate: string;
  mockTargets: Target[];
  mockNamespaces: string[];
  onConfigChange: (updates: Partial<ConfigData>) => void;
  currentTab: string;
  onTabChange: (tab: string) => void;
}

export function ConfigTabs({
  config,
  selectedBoilerplate,
  mockTargets,
  mockNamespaces,
  onConfigChange,
  currentTab,
  onTabChange
}: ConfigTabsProps) {
  return (
    <Tabs value={currentTab} onValueChange={onTabChange} className="space-y-4">
      <TabsList className="grid w-full grid-cols-4">
        <TabsTrigger value="target" className="flex items-center gap-2">
          <HardDrive className="h-4 w-4" />
          Target
        </TabsTrigger>
        <TabsTrigger value="network" className="flex items-center gap-2">
          <Network className="h-4 w-4" />
          Network
        </TabsTrigger>
        <TabsTrigger value="filesystem" className="flex items-center gap-2">
          <HardDrive className="h-4 w-4" />
          FileSystem
        </TabsTrigger>
        <TabsTrigger value="other" className="flex items-center gap-2">
          <Settings2 className="h-4 w-4" />
          Other
        </TabsTrigger>
      </TabsList>

      <TabsContent value="target">
        <TargetSelector
          config={config}
          mockTargets={mockTargets}
          mockNamespaces={mockNamespaces}
          onConfigChange={onConfigChange}
        />
      </TabsContent>

      <TabsContent value="network">
        <NetworkConfig
          selectedBoilerplate={selectedBoilerplate}
          networkConfig={config.network}
          onNetworkChange={(updates) => onConfigChange({ network: { ...config.network, ...updates } })}
        />
      </TabsContent>

      <TabsContent value="filesystem">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="flex items-center gap-2 text-base">
              <HardDrive className="h-4 w-4" />
              File System Configuration
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="flex items-center space-x-2">
              <Switch
                id="fs-enabled"
                checked={config.fileSystem.enabled}
                onCheckedChange={(enabled) =>
                  onConfigChange({
                    fileSystem: { ...config.fileSystem, enabled }
                  })
                }
              />
              <Label htmlFor="fs-enabled">Enable file system mirroring</Label>
            </div>
          </CardContent>
        </Card>
      </TabsContent>

      <TabsContent value="other">
        <div className="space-y-4">
          {/* Environment Configuration */}
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-base">Environment Variables</CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="flex items-center space-x-2">
                <Switch
                  id="env-enabled"
                  checked={config.environment.enabled}
                  onCheckedChange={(enabled) =>
                    onConfigChange({
                      environment: { ...config.environment, enabled }
                    })
                  }
                />
                <Label htmlFor="env-enabled">Enable environment mirroring</Label>
              </div>
              
              {config.environment.enabled && (
                <div className="space-y-3">
                  <div>
                    <Label>Include Pattern</Label>
                    <Input
                      placeholder="VAR1,VAR2*"
                      value={config.environment.include}
                      onChange={(e) =>
                        onConfigChange({
                          environment: { ...config.environment, include: e.target.value }
                        })
                      }
                    />
                  </div>
                  <div>
                    <Label>Exclude Pattern</Label>
                    <Input
                      placeholder="SECRET*"
                      value={config.environment.exclude}
                      onChange={(e) =>
                        onConfigChange({
                          environment: { ...config.environment, exclude: e.target.value }
                        })
                      }
                    />
                  </div>
                </div>
              )}
            </CardContent>
          </Card>

          {/* Agent Configuration */}
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-base">Agent Options</CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="flex items-center space-x-2">
                <Switch
                  id="scaledown"
                  checked={config.agent.scaledown}
                  onCheckedChange={(scaledown) =>
                    onConfigChange({
                      agent: { ...config.agent, scaledown }
                    })
                  }
                />
                <Label htmlFor="scaledown">Scale down target</Label>
              </div>
              
              <div className="flex items-center space-x-2">
                <Switch
                  id="copy-target"
                  checked={config.agent.copyTarget}
                  onCheckedChange={(copyTarget) =>
                    onConfigChange({
                      agent: { ...config.agent, copyTarget }
                    })
                  }
                />
                <Label htmlFor="copy-target">Copy target</Label>
              </div>
            </CardContent>
          </Card>
        </div>
      </TabsContent>
    </Tabs>
  );
}