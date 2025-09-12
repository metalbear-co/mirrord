import { Server, HardDrive } from "lucide-react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

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
}

interface TargetSelectorProps {
  config: ConfigData;
  mockTargets: Target[];
  mockNamespaces: string[];
  onConfigChange: (updates: Partial<ConfigData>) => void;
}

export function TargetSelector({
  config,
  mockTargets,
  mockNamespaces,
  onConfigChange
}: TargetSelectorProps) {
  return (
    <div className="space-y-4">
      <Card>
        <CardHeader className="pb-3">
          <CardTitle className="flex items-center gap-2 text-base">
            <Server className="h-4 w-4" />
            Target Configuration
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="grid grid-cols-2 gap-4">
            <div className="space-y-2">
              <Label htmlFor="config-name">Configuration Name</Label>
              <Input
                id="config-name"
                placeholder="my-config"
                value={config.name}
                onChange={(e) => onConfigChange({ name: e.target.value })}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="namespace">Namespace</Label>
              <Select value={config.namespace} onValueChange={(value) => onConfigChange({ namespace: value })}>
                <SelectTrigger>
                  <SelectValue placeholder="Select namespace" />
                </SelectTrigger>
                <SelectContent>
                  {mockNamespaces.map(ns => (
                    <SelectItem key={ns} value={ns}>{ns}</SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          </div>

          <div className="grid grid-cols-2 gap-4">
            <div className="space-y-2">
              <Label htmlFor="target">Target</Label>
              <Select value={config.target} onValueChange={(value) => onConfigChange({ target: value })}>
                <SelectTrigger>
                  <SelectValue placeholder="Select target" />
                </SelectTrigger>
                <SelectContent>
                  {mockTargets
                    .filter(t => t.namespace === config.namespace)
                    .map(target => (
                      <SelectItem key={`${target.namespace}/${target.name}`} value={target.name}>
                        {target.name} ({target.kind})
                      </SelectItem>
                    ))}
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-2">
              <Label htmlFor="target-type">Target Type</Label> 
              <Select value={config.targetType} onValueChange={(value) => onConfigChange({ targetType: value })}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="deployment">Deployment</SelectItem>
                  <SelectItem value="pod">Pod</SelectItem>
                  <SelectItem value="statefulset">StatefulSet</SelectItem>
                  <SelectItem value="daemonset">DaemonSet</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}