import { useToast } from "@/hooks/use-toast";
import { useState, useContext } from "react";
import {
  Copy,
  Save,
  Server,
  Check,
  AlertCircle,
  ChevronDown,
  Plus,
  Trash2,
  ArrowRight,
  Network,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  CardDescription,
} from "@/components/ui/card";
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
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Separator } from "@/components/ui/separator";
import { Checkbox } from "@/components/ui/checkbox";
import { Textarea } from "@/components/ui/textarea";
import DownloadButton from "../DownloadButton";
import { validateJson } from "../JsonUtils";
import { ConfigDataContext } from "../UserDataContext";

const ConfigTabs = () => {
  const config = useContext(ConfigDataContext);
  const [currentTab, setCurrentTab] = useState<string>("target");

  // TODO: write a util function to determine selectedBoilerplate from config
  const selectedBoilerplate: "steal" | "mirror" | "replace" = "steal";

  // TODO: remove all this editable json stuff, replace with using config directly
  // For copying to clipboard
  const { toast } = useToast();
  const copyToClipboard = async () => {
    const jsonToCopy = editableJson || config.config;
    await navigator.clipboard.writeText(jsonToCopy);
    toast({
      title: "Copied to clipboard",
      description: "Configuration JSON has been copied to your clipboard.",
    });
  };
  const [editableJson, setEditableJson] = useState<string>("");
  const [jsonError, setJsonError] = useState<string>("");

  // todo: use the actual change config from config.whatever
  const setConfig = (newConfig: any) => {
    config.setConfig(newConfig)
    return;
  };

  // todo: pull targets from backend
  const mockTargets = [
    {
      name: "api-service",
      namespace: "default",
      kind: "deployment",
    },
    {
      name: "frontend-app",
      namespace: "default",
      kind: "deployment",
    },
    {
      name: "database",
      namespace: "production",
      kind: "statefulset",
    },
    {
      name: "worker-queue",
      namespace: "default",
      kind: "deployment",
    },
    {
      name: "backup-job",
      namespace: "system",
      kind: "cronjob",
    },
  ];

  const mockNamespaces = [
    "default",
    "production",
    "system",
    "kube-system",
    "monitoring",
  ];

  return (
    <>
      {/* Tabs navigation above configuration options */}
      <Tabs
        value={currentTab}
        onValueChange={(value) => {
          // Only allow navigation if required fields are filled
          if (value === "network" && !config.config.target) return;
          if (value === "export" && !config.config.target) return;
          setCurrentTab(value);
        }}
        className="w-full"
      >
        <TabsList className="grid w-full grid-cols-3 h-10">
          <TabsTrigger value="target" className="text-sm">
            Target
          </TabsTrigger>
          <TabsTrigger
            value="network"
            className="text-sm"
            disabled={!config.config.target}
          >
            Network
          </TabsTrigger>
          <TabsTrigger
            value="export"
            className="text-sm"
            disabled={!config.config.target}
          >
            Export
          </TabsTrigger>
        </TabsList>
      </Tabs>
      {/* Main configuration options */}
      <Tabs
        value={currentTab}
        onValueChange={(value) => {
          if (value === "network" && !config.config.target) return;
          if (value === "export" && !config.config.target) return;
          setCurrentTab(value);
        }}
        className="w-full"
      >
        <TabsContent value="target" className="space-y-3 mt-4">
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="flex items-center gap-2 text-base">
                <Server className="h-4 w-4" />
                Target Selection
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              <div>
                <Label htmlFor="namespace">Namespace</Label>
                <Select
                  value={config.config.namespace}
                  onValueChange={(value) =>
                    setConfig({
                      ...config.config,
                      target: {
                        ...config.config.target,
                        namespace: value,
                      },
                    })
                  }
                >
                  <SelectTrigger>
                    <SelectValue placeholder="Select a namespace" />
                  </SelectTrigger>
                  <SelectContent>
                    {mockNamespaces.map((namespace) => (
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
                  value={config.config.targetType}
                  onValueChange={(value) =>
                    setConfig({
                      ...config,
                      targetType: value,
                    })
                  }
                >
                  <SelectTrigger>
                    <SelectValue placeholder="Select resource type" />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="deployment">Deployment</SelectItem>
                    <SelectItem value="statefulset">StatefulSet</SelectItem>
                    <SelectItem value="daemonset">DaemonSet</SelectItem>
                    <SelectItem value="pod">Pod</SelectItem>
                    <SelectItem value="cronjob">CronJob</SelectItem>
                    <SelectItem value="job">Job</SelectItem>
                  </SelectContent>
                </Select>
              </div>

              <div>
                <Label htmlFor="target-search">Choose Target</Label>
                <Popover>
                  <PopoverTrigger asChild>
                    <Button
                      variant="outline"
                      className="w-full justify-between"
                    >
                      {(config.config.target?? false) && config.config.target.path
                        ? config.config.target.path.split("/")[2] ||
                          config.config.target.path
                        : "Search for target..."}
                      <ChevronDown className="h-4 w-4 opacity-50" />
                    </Button>
                  </PopoverTrigger>
                  <PopoverContent
                    className="p-0 w-[--radix-popover-trigger-width]"
                    align="start"
                  >
                    <div className="p-2">
                      <Input placeholder="Search targets..." className="mb-2" />
                      <div className="max-h-48 overflow-y-auto space-y-1">
                        {mockTargets
                          .filter(
                            (target) =>
                              target.kind === config.config.targetType &&
                              target.namespace === config.config.namespace
                          )
                          .map((target) => (
                            <div
                              key={`${target.namespace}/${target.name}`}
                              className="flex items-center justify-between p-2 hover:bg-muted rounded-md cursor-pointer"
                              onClick={() => {
                                const targetValue = `${target.namespace}/${target.kind}/${target.name}`;
                                const configType =
                                  selectedBoilerplate || "config";
                                setConfig({
                                  ...config,
                                  target: targetValue,
                                  name: `${configType}-config-${target.name}`,
                                });
                                document.dispatchEvent(
                                  new KeyboardEvent("keydown", {
                                    key: "Escape",
                                  })
                                );
                              }}
                            >
                              <div className="flex flex-col">
                                <span className="font-medium">
                                  {target.name}
                                </span>
                              </div>
                              <Badge variant="outline">{target.kind}</Badge>
                            </div>
                          ))}
                      </div>
                    </div>
                  </PopoverContent>
                </Popover>
                {!config.config.target && (
                  <p className="text-sm text-destructive flex items-center gap-1 mt-1">
                    <AlertCircle className="h-4 w-4" />
                    Please select a target to continue
                  </p>
                )}
              </div>

              <div>
                <Label htmlFor="config-name">Configuration Name</Label>
                <Input
                  id="config-name"
                  placeholder="e.g., prod-api-config"
                  value={config.config.name}
                  onChange={(e) =>
                    setConfig({
                      ...config,
                      name: e.target.value,
                    })
                  }
                />
                <p className="text-sm text-muted-foreground mt-1">
                  This name will be used to identify your configuration
                </p>
              </div>
            </CardContent>
          </Card>
        </TabsContent>

        <TabsContent value="network" className="space-y-3 mt-4">
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="flex items-center gap-2 text-base">
                <Network className="h-4 w-4" />
                Traffic Filtering
              </CardTitle>
            </CardHeader>
            <CardContent>
              <div className="space-y-4">
                <div className="space-y-4">
                  <div>
                    <p className="text-sm text-muted-foreground mb-4">
                      mirrord supports stealing a subset of the remote target's
                      traffic. You can do this by specifying a filter on either
                      an HTTP header or path.
                    </p>
                  </div>

                  {/* HTTP Filters */}
                  <div className="space-y-4">
                    <div className="space-y-3">
                      {/* Header Filtering */}
                      <div className="space-y-2">
                        <div className="flex items-center justify-between">
                          <Label className="font-medium">Header Filters</Label>
                          <Button
                            type="button"
                            variant="outline"
                            size="sm"
                            onClick={() =>
                              setConfig({
                                ...config,
                                network: {
                                  ...config.config.feature.network,
                                  incoming: {
                                    ...config.config.feature.network.incoming,
                                    httpFilter: [
                                      ...config.config.feature.network.incoming
                                        .httpFilter,
                                      {
                                        type: "header",
                                        value: "",
                                        matchType: "exact",
                                      },
                                    ],
                                  },
                                },
                              })
                            }
                          >
                            <Plus className="h-4 w-4 mr-1" />
                            Add
                          </Button>
                        </div>
                        (
                        {(
                          config.config.feature.network.incoming.httpFilter ??
                          []
                        ).filter((f) => f.type === "header").length > 0 && (
                          <div className="space-y-3">
                            {(
                              config.config.feature.network.incoming
                                .httpFilter ?? []
                            )
                              .filter((f) => f.type === "header")
                              .map((filter, index) => (
                                <div key={index} className="space-y-2">
                                  <div className="flex items-center gap-2">
                                    <Input
                                      placeholder="e.g., x-mirrord-test: true"
                                      value={filter.value}
                                      onChange={(e) => {
                                        const newFilters = [
                                          ...config.config.feature.network
                                            .incoming.httpFilter,
                                        ];
                                        const headerIndex =
                                          newFilters.findIndex(
                                            (f, i) =>
                                              f.type === "header" && i === index
                                          );
                                        if (headerIndex !== -1) {
                                          newFilters[headerIndex] = {
                                            ...newFilters[headerIndex],
                                            value: e.target.value,
                                          };
                                          setConfig({
                                            ...config,
                                            network: {
                                              ...config.config.feature.network,
                                              incoming: {
                                                ...config.config.feature.network
                                                  .incoming,
                                                httpFilter: newFilters,
                                              },
                                            },
                                          });
                                        }
                                      }}
                                      className="flex-1"
                                    />
                                    <Select
                                      value={filter.matchType || "exact"}
                                      onValueChange={(
                                        value: "exact" | "regex"
                                      ) => {
                                        const newFilters = [
                                          ...config.config.feature.network
                                            .incoming.httpFilter,
                                        ];
                                        const headerIndex =
                                          newFilters.findIndex(
                                            (f, i) =>
                                              f.type === "header" && i === index
                                          );
                                        if (headerIndex !== -1) {
                                          newFilters[headerIndex] = {
                                            ...newFilters[headerIndex],
                                            matchType: value,
                                          };
                                          setConfig({
                                            ...config,
                                            network: {
                                              ...config.config.feature.network,
                                              incoming: {
                                                ...config.config.feature.network
                                                  .incoming,
                                                httpFilter: newFilters,
                                              },
                                            },
                                          });
                                        }
                                      }}
                                    >
                                      <SelectTrigger className="w-20">
                                        <SelectValue />
                                      </SelectTrigger>
                                      <SelectContent>
                                        <SelectItem value="exact">
                                          Exact
                                        </SelectItem>
                                        <SelectItem value="regex">
                                          Regex
                                        </SelectItem>
                                      </SelectContent>
                                    </Select>
                                    <Button
                                      type="button"
                                      variant="outline"
                                      size="sm"
                                      onClick={() => {
                                        const newFilters = (
                                          config.config.feature.network.incoming
                                            .httpFilter ?? []
                                        ).filter(
                                          (f, i) =>
                                            !(
                                              f.type === "header" && i === index
                                            )
                                        );
                                        setConfig({
                                          ...config,
                                          network: {
                                            ...config.config.feature.network,
                                            incoming: {
                                              ...config.config.feature.network
                                                .incoming,
                                              httpFilter: newFilters,
                                            },
                                          },
                                        });
                                      }}
                                    >
                                      <Trash2 className="h-4 w-4" />
                                    </Button>
                                  </div>
                                </div>
                              ))}
                          </div>
                        )}
                        )
                      </div>

                      {/* Path Filtering */}
                      <div className="space-y-2">
                        <div className="flex items-center justify-between">
                          <Label className="font-medium">Path Filters</Label>
                          <Button
                            type="button"
                            variant="outline"
                            size="sm"
                            onClick={() =>
                              setConfig({
                                ...config,
                                network: {
                                  ...config.config.feature.network,
                                  incoming: {
                                    ...config.config.feature.network.incoming,
                                    httpFilter: [
                                      ...config.config.feature.network.incoming
                                        .httpFilter,
                                      {
                                        type: "path",
                                        value: "",
                                      },
                                    ],
                                  },
                                },
                              })
                            }
                          >
                            <Plus className="h-4 w-4 mr-1" />
                            Add
                          </Button>
                        </div>

                        {(
                          config.config.feature.network.incoming.httpFilter ??
                          []
                        ).filter((f) => f.type === "path").length > 0 && (
                          <div className="space-y-2">
                            {(
                              config.config.feature.network.incoming
                                .httpFilter ?? []
                            )
                              .filter((f) => f.type === "path")
                              .map((filter, index) => (
                                <div
                                  key={index}
                                  className="flex items-center gap-2"
                                >
                                  <Input
                                    placeholder="e.g., /api/v1/test"
                                    value={filter.value}
                                    onChange={(e) => {
                                      const newFilters = [
                                        ...config.config.feature.network
                                          .incoming.httpFilter,
                                      ];
                                      const pathIndex = newFilters.findIndex(
                                        (f, i) =>
                                          f.type === "path" && i === index
                                      );
                                      if (pathIndex !== -1) {
                                        newFilters[pathIndex] = {
                                          ...newFilters[pathIndex],
                                          value: e.target.value,
                                        };
                                        setConfig({
                                          ...config,
                                          network: {
                                            ...config.config.feature.network,
                                            incoming: {
                                              ...config.config.feature.network
                                                .incoming,
                                              httpFilter: newFilters,
                                            },
                                          },
                                        });
                                      }
                                    }}
                                  />
                                  <Button
                                    type="button"
                                    variant="outline"
                                    size="sm"
                                    onClick={() => {
                                      const newFilters = (
                                        config.config.feature.network.incoming
                                          .httpFilter ?? []
                                      ).filter(
                                        (f, i) =>
                                          !(f.type === "path" && i === index)
                                      );
                                      setConfig({
                                        ...config,
                                        network: {
                                          ...config.config.feature.network,
                                          incoming: {
                                            ...config.config.feature.network
                                              .incoming,
                                            httpFilter: newFilters,
                                          },
                                        },
                                      });
                                    }}
                                  >
                                    <Trash2 className="h-4 w-4" />
                                  </Button>
                                </div>
                              ))}
                          </div>
                        )}
                      </div>

                      {/* Filter Logic Selection - Only show when there are multiple filters */}
                      {(config.config.feature.network.incoming.httpFilter ?? [])
                        .length > 1 && (
                        <>
                          <Separator />
                          <div className="space-y-2">
                            <Label className="font-medium">Filter Logic</Label>
                            <RadioGroup
                              value={
                                config.config.feature.network.incoming
                                  .filterOperator
                              }
                              onValueChange={(value: "AND" | "OR") =>
                                setConfig({
                                  ...config,
                                  network: {
                                    ...config.config.feature.network,
                                    incoming: {
                                      ...config.config.feature.network.incoming,
                                      filterOperator: value,
                                    },
                                  },
                                })
                              }
                            >
                              <div className="flex items-center space-x-2">
                                <RadioGroupItem value="AND" id="and" />
                                <Label htmlFor="and" className="text-sm">
                                  <strong>All</strong> - Match all specified
                                  filters
                                </Label>
                              </div>
                              <div className="flex items-center space-x-2">
                                <RadioGroupItem value="OR" id="or" />
                                <Label htmlFor="or" className="text-sm">
                                  <strong>Any</strong> - Match any specified
                                  filter
                                </Label>
                              </div>
                            </RadioGroup>
                          </div>
                        </>
                      )}
                    </div>
                  </div>

                  {/* Simplified Port Configuration */}
                  <div className="space-y-4 mt-6">
                    <div>
                      <h3 className="text-base font-semibold mb-1">
                        Port Configuration
                      </h3>
                      <p className="text-xs text-muted-foreground mb-3">
                        Click a port to enable HTTP filtering. Deselect any
                        ports that are not HTTP or gRPC. The following ports
                        were detected in your target, with some already
                        preselected by default.
                      </p>
                    </div>

                    <div className="space-y-4">
                      {/* Detected Ports */}
                      <div>
                        <div className="flex flex-wrap gap-2">
                          {[
                            "8080",
                            "3000",
                            "5432",
                            "9000",
                            "4000",
                            "6379",
                            "5672",
                            "3306",
                          ].map((port) => {
                            const isSelected = (
                              config.config.feature.network.incoming.ports ?? []
                            ).some((p) => p.remote === port);
                            const togglePort = () => {
                              if (isSelected) {
                                // Remove port
                                const newPorts = (
                                  config.config.feature.network.incoming
                                    .ports ?? []
                                ).filter((p) => p.remote !== port);
                                setConfig({
                                  ...config,
                                  network: {
                                    ...config.config.feature.network,
                                    incoming: {
                                      ...config.config.feature.network.incoming,
                                      ports: newPorts,
                                    },
                                  },
                                });
                              } else {
                                // Add port
                                setConfig({
                                  ...config,
                                  network: {
                                    ...config.config.feature.network,
                                    incoming: {
                                      ...config.config.feature.network.incoming,
                                      ports: [
                                        ...config.config.feature.network
                                          .incoming.ports,
                                        {
                                          remote: port,
                                          local: port,
                                        },
                                      ],
                                    },
                                  },
                                });
                              }
                            };
                            return (
                              <Button
                                key={port}
                                variant={isSelected ? "default" : "outline"}
                                size="sm"
                                className={`rounded-full px-4 py-2 font-mono transition-all ${
                                  isSelected
                                    ? "bg-primary text-primary-foreground hover:bg-primary/90"
                                    : "hover:bg-primary/10"
                                }`}
                                onClick={togglePort}
                              >
                                {port}
                              </Button>
                            );
                          })}
                        </div>
                      </div>

                      {/* Port Mappings */}
                      {(config.config.feature.network.incoming.ports ?? [])
                        .length > 0 && (
                        <div className="space-y-3">
                          <h4 className="text-base font-semibold">
                            Selected Ports
                          </h4>
                          <p className="text-sm text-muted-foreground mb-3">
                            Only map ports that run on different ports locally.
                          </p>
                          <div className="space-y-3">
                            {(
                              config.config.feature.network.incoming.ports ?? []
                            ).map((portConfig, index) => (
                              <div
                                key={portConfig.remote}
                                className="border rounded-lg p-4 space-y-3"
                              >
                                <div className="flex items-center justify-between">
                                  <div className="flex items-center gap-2">
                                    <Badge
                                      variant="outline"
                                      className="font-mono"
                                    >
                                      {portConfig.remote}
                                    </Badge>
                                    <span className="text-sm text-muted-foreground">
                                      Remote Port
                                    </span>
                                  </div>
                                </div>

                                <div className="flex items-center space-x-2">
                                  <Checkbox
                                    id={`local-port-${portConfig.remote}`}
                                    checked={
                                      portConfig.local !== portConfig.remote
                                    }
                                    onCheckedChange={(checked) => {
                                      const newPorts = [
                                        ...config.config.feature.network
                                          .incoming.ports,
                                      ];
                                      newPorts[index] = {
                                        ...newPorts[index],
                                        local: checked ? "" : portConfig.remote,
                                      };
                                      setConfig({
                                        ...config,
                                        network: {
                                          ...config.config.feature.network,
                                          incoming: {
                                            ...config.config.feature.network
                                              .incoming,
                                            ports: newPorts,
                                          },
                                        },
                                      });
                                    }}
                                  />
                                  <Label
                                    htmlFor={`local-port-${portConfig.remote}`}
                                    className="text-sm"
                                  >
                                    Local port is different than remote
                                  </Label>
                                </div>

                                {portConfig.local !== portConfig.remote && (
                                  <div className="flex items-center gap-3">
                                    <div className="flex-1">
                                      <Label className="text-xs text-muted-foreground">
                                        Local Port
                                      </Label>
                                      <Input
                                        className="font-mono"
                                        placeholder={portConfig.remote}
                                        value={portConfig.local}
                                        onChange={(e) => {
                                          const newPorts = [
                                            ...config.config.feature.network
                                              .incoming.ports,
                                          ];
                                          newPorts[index] = {
                                            ...newPorts[index],
                                            local:
                                              e.target.value ||
                                              portConfig.remote,
                                          };
                                          setConfig({
                                            ...config,
                                            network: {
                                              ...config.config.feature.network,
                                              incoming: {
                                                ...config.config.feature.network
                                                  .incoming,
                                                ports: newPorts,
                                              },
                                            },
                                          });
                                        }}
                                      />
                                    </div>

                                    <ArrowRight className="h-4 w-4 text-muted-foreground flex-shrink-0 mt-4" />

                                    <div className="flex-1">
                                      <Label className="text-xs text-muted-foreground">
                                        Remote Port
                                      </Label>
                                      <Input
                                        className="font-mono"
                                        value={portConfig.remote}
                                        readOnly
                                      />
                                    </div>
                                  </div>
                                )}
                              </div>
                            ))}
                          </div>
                        </div>
                      )}
                    </div>
                  </div>
                </div>
              </div>
            </CardContent>
          </Card>
        </TabsContent>

        <TabsContent value="export" className="space-y-4 mt-6">
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <Save className="h-5 w-5" />
                Export Configuration
              </CardTitle>
              <CardDescription>
                Edit and export your mirrord.json configuration
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="space-y-2">
                <Label htmlFor="json-editor">Configuration JSON</Label>
                <div className="relative">
                  <Textarea
                    id="json-editor"
                    className="font-mono text-sm min-h-[200px] max-h-[200px] resize-none"
                    // value={editableJson || generateConfigJson(config)}
                    value=""
                    onChange={(e) => {
                      setEditableJson(e.target.value);
                      validateJson(e.target.value, setJsonError);
                    }}
                    placeholder='{\n     "target": {\n         "path": "deployment/api-service",\n         "namespace": "default"\n     },\n     "feature": {\n         "fs": "read",\n         "env": true,\n         "copy_target": {\n             "scale_down": true\n         }\n     }\n }'
                  />
                  {jsonError && (
                    <div className="absolute bottom-2 left-2 flex items-center gap-1 text-destructive text-xs bg-background/80 px-2 py-1 rounded">
                      <AlertCircle className="h-3 w-3" />
                      {jsonError}
                    </div>
                  )}
                </div>
              </div>

              <div className="flex flex-wrap gap-2">
                <Button
                  variant="outline"
                  size="sm"
                  onClick={copyToClipboard}
                  disabled={!!jsonError}
                >
                  <Copy className="h-4 w-4 mr-2" />
                  Copy to Clipboard
                </Button>

                <DownloadButton
                  // json={editableJson || generateConfigJson(config)}
                  json="string" // TODO: config.config.toString()
                  filename={"mirrord-config"} // TODO: use config name
                />

                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    setEditableJson(config.config);
                    setJsonError("");
                  }}
                >
                  Reset to Generated
                </Button>
              </div>

              <Separator />

              <div className="space-y-3">
                <h4 className="font-medium">How to use your configuration:</h4>
                <div className="space-y-2 text-sm text-muted-foreground">
                  <p>
                    <strong>CLI:</strong> Use the{" "}
                    <code className="px-1 py-0.5 bg-muted rounded text-xs">
                      -f &lt;CONFIG_PATH&gt;
                    </code>{" "}
                    flag
                  </p>
                  <p>
                    <strong>VSCode Extension or JetBrains plugin:</strong>{" "}
                    Create a{" "}
                    <code className="px-1 py-0.5 bg-muted rounded text-xs">
                      .mirrord/mirrord.json
                    </code>{" "}
                    file or use the UI
                  </p>
                  <p>
                    For more details, see the{" "}
                    <a
                      href="https://metalbear.co/mirrord/docs/config#basic-mirrord.json-with-templating"
                      target="_blank"
                      rel="noopener noreferrer"
                      className="text-primary hover:underline"
                    >
                      configuration documentation
                    </a>
                  </p>
                </div>
              </div>
            </CardContent>
          </Card>
        </TabsContent>
      </Tabs>
    </>
  );
};

export default ConfigTabs;
