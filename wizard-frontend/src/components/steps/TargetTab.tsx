import { useState, useContext } from "react";
import { Server, AlertCircle, ChevronDown } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { TabsContent } from "@/components/ui/tabs";
import {
  readCurrentTargetDetails,
  updateConfigPorts,
  updateConfigTarget,
} from "../JsonUtils";
import { ConfigDataContext } from "../UserDataContext";
import { useQuery } from "@tanstack/react-query";

// interface ClusterDetails {
//   namespaces: string[];
//   target_types: string[];
// }

// interface Target {
//   target_path: string;
//   target_namespace: string;
//   detected_ports: number[];
// }

const TargetTab = ({ setTargetPorts }: { setTargetPorts }) => {
  const { config, setConfig } = useContext(ConfigDataContext)!;
  const [namespace, setNamespace] = useState<string>("default");
  const [targetType, setTargetType] = useState<string>("");
  const [targetSearchText, setTargetSearchText] = useState<string>("");

  // refresh cluster details every 30 seconds
  const clusterDetailsQuery = useQuery({
    staleTime: 30 * 1000,
    queryKey: ["clusterDetails"],
    queryFn: () =>
      fetch(window.location.href + "api/v1/cluster-details").then(
        async (res) =>
          res.ok ? await res.json() : { namespaces: [], target_types: [] },
      ),
  });

  if (clusterDetailsQuery.error) console.log(clusterDetailsQuery.error);

  const availableNamespaces =
    clusterDetailsQuery.isLoading || clusterDetailsQuery.error
      ? []
      : clusterDetailsQuery.data.namespaces;
  const availableTargetTypes =
    clusterDetailsQuery.isLoading || clusterDetailsQuery.error
      ? []
      : clusterDetailsQuery.data.target_types;

  const targetsQuery = useQuery({
    queryKey: ["targetDetails", namespace, targetType],
    queryFn: () =>
      fetch(
        window.location.href +
          "api/v1/namespace/" +
          namespace +
          "/targets" +
          (targetType ? "?target_type=" + targetType : ""),
      ).then(async (res) => (res.ok ? await res.json() : [])),
  });

  if (targetsQuery.error) console.log(targetsQuery.error);

  const availableTargets =
    targetsQuery.isLoading || targetsQuery.error ? [] : targetsQuery.data;

  return (
    <TabsContent value="target" className="space-y-3 mt-4">
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="flex items-center gap-2 text-base">
            <Server className="h-4 w-4" />
            Target Selection
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3 text-left">
          <div>
            <Label htmlFor="namespace">Namespace</Label>
            <Select
              value={namespace}
              onValueChange={(value) => setNamespace(value)}
            >
              <SelectTrigger>
                <SelectValue placeholder="Select a namespace" />
              </SelectTrigger>
              <SelectContent>
                {availableNamespaces.map((namespace) => (
                  <SelectItem key={namespace} value={namespace}>
                    {namespace}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          <div>
            <Label htmlFor="target-type">Target Type</Label>
            <Select
              value={targetType}
              onValueChange={(value) => setTargetType(value)}
            >
              <SelectTrigger>
                <SelectValue placeholder="Select resource type" />
              </SelectTrigger>
              <SelectContent>
                {availableTargetTypes.map((ttype) => {
                  return <SelectItem value={ttype}>{ttype}</SelectItem>;
                })}
              </SelectContent>
            </Select>
          </div>

          <div>
            <Label htmlFor="target-search">Choose Target</Label>
            <Popover>
              <PopoverTrigger asChild>
                <Button variant="outline" className="w-full justify-between">
                  {readCurrentTargetDetails(config).name ??
                    "Search for target..."}
                  <ChevronDown className="h-4 w-4 opacity-50" />
                </Button>
              </PopoverTrigger>
              <PopoverContent
                className="p-0 w-[--radix-popover-trigger-width]"
                align="start"
              >
                <div className="p-2">
                  <Input
                    placeholder="Search targets..."
                    className="mb-2"
                    onChange={(event) => {
                      setTargetSearchText(event.target.value);
                    }}
                  />
                  <div className="max-h-48 overflow-y-auto space-y-1">
                    {availableTargets
                      .filter((target) => {
                        return target.target_path.includes(targetSearchText);
                      })
                      .map((target) => (
                        <div
                          key={`${target.target_namespace}/${target.target_path}`}
                          className="flex items-center justify-between p-2 hover:bg-muted rounded-md cursor-pointer"
                          onClick={() => {
                            const updated = updateConfigTarget(
                              config,
                              target.target_path,
                              target.target_namespace,
                            );

                            // set target ports for port config in network tab
                            setTargetPorts(target.detected_ports);
                            // when the target changes, reset ports config
                            const updatedPorts = updateConfigPorts(
                              target.detected_ports,
                              updated,
                            );
                            setConfig(updatedPorts);

                            document.dispatchEvent(
                              new KeyboardEvent("keydown", {
                                key: "Escape",
                              }),
                            );
                          }}
                        >
                          <div className="flex flex-col">
                            <span className="font-medium">
                              {target.target_path.split("/")[1]}
                            </span>
                          </div>
                          <Badge variant="outline">
                            {target.target_path.split("/")[0]}
                          </Badge>
                        </div>
                      ))}
                  </div>
                </div>
              </PopoverContent>
            </Popover>
            {!config.target && (
              <p className="text-sm text-destructive flex items-center gap-1 mt-1">
                <AlertCircle className="h-4 w-4" />
                Please select a target to continue
              </p>
            )}
          </div>
        </CardContent>
      </Card>
    </TabsContent>
  );
};

export default TargetTab;
