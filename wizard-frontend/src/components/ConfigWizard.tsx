import { useState, useRef, useEffect } from "react";
import {
  Filter,
  Copy,
  Repeat,
  ChevronRight,
  ChevronLeft,
  Save,
  BookOpen,
  SkipForward,
  Server,
  Check,
  AlertCircle,
  ChevronDown,
  Play,
  Download,
  Plus,
  Trash2,
  ArrowRight,
  HardDrive,
  Network,
  Settings2,
} from "lucide-react";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  CardDescription,
} from "@/components/ui/card";
import BoilerplateCard, { BoilerplateCardProps } from "./BoilerplateCard";
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
import { Switch } from "@/components/ui/switch";
import { useToast } from "@/hooks/use-toast";
import DownloadButton from "./DownloadButton";
import { generateConfigJson, validateJson, updateConfigFromJson } from "./JsonUtils";
import { WizardHeader } from "./WizardHeader";
import { WizardFooter } from "./WizardFooter";
import type {
  FeatureConfig,
  ConfigData as SharedConfigData,
} from "@/types/config";
type ConfigData = SharedConfigData;

interface ConfigWizardProps {
  isOpen: boolean;
  onClose: () => void;
  onSave: (config: ConfigData) => void;
  isOverview?: boolean;
  isReturning: boolean;
}

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

type OnboardingStep =
  | "intro"
  | "explanation"
  | "boilerplate"
  | "config";

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
    onClick: () => { },
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
    onClick: () => { },
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
    onClick: () => { },
  },
];

export function ConfigWizard({
  isOpen,
  onClose,
  onSave,
  isOverview = false,
  isReturning,
}: ConfigWizardProps) {
  const [onboardingStep, setOnboardingStep] = useState<OnboardingStep>(
    isOverview ? "intro" : "boilerplate"
  );
  const [selectedBoilerplate, setSelectedBoilerplate] = useState<string>("");
  const [filterType, setFilterType] = useState<"header" | "path">("header");
  const [copied, setCopied] = useState(false);
  const [editableJson, setEditableJson] = useState<string>("");
  const [jsonError, setJsonError] = useState<string>("");
  const [currentTab, setCurrentTab] = useState<string>("target");
  const [showPortMapping, setShowPortMapping] = useState<boolean>(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const { toast } = useToast();
  const [config, setConfig] = useState<ConfigData>({
    name: "",
    target: "",
    targetType: "deployment",
    namespace: "default",
    fileSystem: {
      enabled: false,
      mode: "read",
      rules: [
        {
          mode: "read",
          filter: "",
        },
      ],
    },
    network: {
      incoming: {
        enabled: true,
        mode: "mirror",
        httpFilter: [],
        filterOperator: "AND",
        ports: [],
      },
      outgoing: {
        enabled: true,
        protocol: "both",
        filter: "",
        filterTarget: "remote",
      },
      dns: {
        enabled: false,
        filter: "",
      },
    },
    environment: {
      enabled: false,
      include: "",
      exclude: "",
      override: "",
    },
    agent: {
      scaledown: false,
      copyTarget: false,
    },
    isActive: false,
  });

  // Auto-scroll to bottom when content changes
  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [
    config.network.incoming.ports,
    config.network.incoming.httpFilter,
    config.fileSystem.rules,
  ]);
  const handleBoilerplateSelect = (boilerplateId: string) => {
    setSelectedBoilerplate(boilerplateId);
    const baseConfig = {
      ...config,
      name: `${boilerplateId}-config`,
      targetType: "deployment",
    };
    switch (boilerplateId) {
      case "replace":
        setConfig({
          ...baseConfig,
          network: {
            ...baseConfig.network,
            incoming: {
              ...baseConfig.network.incoming,
              mode: "steal",
            },
          },
          agent: {
            scaledown: true,
            copyTarget: true,
          },
        });
        break;
      case "mirror":
        setConfig({
          ...baseConfig,
          network: {
            ...baseConfig.network,
            incoming: {
              ...baseConfig.network.incoming,
              mode: "mirror",
            },
          },
          agent: {
            scaledown: false,
            copyTarget: false,
          },
        });
        break;
      case "steal":
        setConfig({
          ...baseConfig,
          network: {
            ...baseConfig.network,
            incoming: {
              ...baseConfig.network.incoming,
              mode: "steal",
            },
          },
          agent: {
            scaledown: false,
            copyTarget: false,
          },
        });
        break;
    }
    // Automatically advance to config step
    setOnboardingStep("config");
  };
  const handleFollowUpComplete = () => {
    if (selectedBoilerplate === "steal") {
      const filterValue =
        filterType === "header" ? "x-mirrord-test: true" : "/api/v1/test";
      setConfig({
        ...config,
        network: {
          ...config.network,
          incoming: {
            ...config.network.incoming,
            httpFilter: [
              {
                type: filterType,
                value: filterValue,
              },
            ],
          },
        },
      });
    }
    setOnboardingStep("config");
  };

  const copyToClipboard = async () => {
    const jsonToCopy = editableJson || generateConfigJson(config);
    await navigator.clipboard.writeText(jsonToCopy);
    setCopied(true);
    toast({
      title: "Copied to clipboard",
      description: "Configuration JSON has been copied to your clipboard.",
    });
    setTimeout(() => setCopied(false), 2000);
  };

  if (!isOpen) return null;

  // TODO: force into WizardStep
  const introStep = (
    <div className="space-y-6">
      <div className="text-center space-y-4">
        <div className="mx-auto w-24 h-24 bg-gradient-to-br from-primary to-primary/60 rounded-full flex items-center justify-center">
          <Server className="h-12 w-12 text-white" />
        </div>

        <div className="space-y-2">
          <h3 className="text-lg font-semibold">
            Get started with mirrord
          </h3>
          <p className="text-muted-foreground max-w-md mx-auto">
            Run local code like it's in your Kubernetes cluster
            without deploying it first. Get started by creating your
            first mirrord.json configuration.
          </p>
        </div>
      </div>
    </div>
  );

  return (
    <Dialog open={isOpen} onOpenChange={onClose}>
      <DialogContent className="max-w-2xl max-h-[85vh] p-0 flex flex-col">
        {/* Fixed Header */}
        <WizardHeader
          isReturning={isReturning}
          onboardingStep={onboardingStep}
          selectedBoilerplate={selectedBoilerplate}
          boilerplateConfigs={boilerplateConfigs}
          currentTab={currentTab}
          onTabChange={(value) => {
            // Only allow navigation if required fields are filled
            if (value === "network" && !config.target) return;
            if (value === "export" && !config.target) return;
            setCurrentTab(value);
          }}
          configTarget={config.target}
        />

        {/* Scrollable Content */}
        <ScrollArea
          className="flex-1 px-4 pt-2 pb-4 overflow-y-auto"
          ref={scrollRef}
        >
          {/* First-time user flow */}
          {!isReturning && (
            <>
              {onboardingStep === "intro" && introStep}

              {onboardingStep === "explanation" && (
                <div className="space-y-6">
                  <div className="text-center space-y-4 pt-4">
                    <p className="text-sm text-muted-foreground">
                      Skip this if you're already familiar with mirrord and just
                      want to create a config file
                    </p>
                  </div>
                </div>
              )}

              {onboardingStep === "boilerplate" && (
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
              )}
            </>
          )}

          {/* Configuration interface (both flows) */}
          {(isReturning || onboardingStep === "config") && (
            <>
              <Tabs
                value={currentTab}
                onValueChange={(value) => {
                  // Only allow navigation if required fields are filled
                  if (value === "network" && !config.target) return;
                  if (value === "export" && !config.target) return;
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
                          value={config.namespace}
                          onValueChange={(value) =>
                            setConfig({
                              ...config,
                              namespace: value,
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
                          value={config.targetType}
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
                            <SelectItem value="deployment">
                              Deployment
                            </SelectItem>
                            <SelectItem value="statefulset">
                              StatefulSet
                            </SelectItem>
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
                              {config.target
                                ? config.target.split("/")[2] || config.target
                                : "Search for target..."}
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
                              />
                              <div className="max-h-48 overflow-y-auto space-y-1">
                                {mockTargets
                                  .filter(
                                    (target) =>
                                      target.kind === config.targetType &&
                                      target.namespace === config.namespace
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
                                      <Badge variant="outline">
                                        {target.kind}
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

                      <div>
                        <Label htmlFor="config-name">Configuration Name</Label>
                        <Input
                          id="config-name"
                          placeholder="e.g., prod-api-config"
                          value={config.name}
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
                        {selectedBoilerplate === "steal" && (
                          <div className="space-y-4">
                            <div>
                              <p className="text-sm text-muted-foreground mb-4">
                                mirrord supports stealing a subset of the remote
                                target's traffic. You can do this by specifying
                                a filter on either an HTTP header or path.
                              </p>
                            </div>

                            {/* HTTP Filters */}
                            <div className="space-y-4">
                              <div className="space-y-3">
                                {/* Header Filtering */}
                                <div className="space-y-2">
                                  <div className="flex items-center justify-between">
                                    <Label className="font-medium">
                                      Header Filters
                                    </Label>
                                    <Button
                                      type="button"
                                      variant="outline"
                                      size="sm"
                                      onClick={() =>
                                        setConfig({
                                          ...config,
                                          network: {
                                            ...config.network,
                                            incoming: {
                                              ...config.network.incoming,
                                              httpFilter: [
                                                ...config.network.incoming
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

                                  {config.network.incoming.httpFilter.filter(
                                    (f) => f.type === "header"
                                  ).length > 0 && (
                                      <div className="space-y-3">
                                        {config.network.incoming.httpFilter
                                          .filter((f) => f.type === "header")
                                          .map((filter, index) => (
                                            <div
                                              key={index}
                                              className="space-y-2"
                                            >
                                              <div className="flex items-center gap-2">
                                                <Input
                                                  placeholder="e.g., x-mirrord-test: true"
                                                  value={filter.value}
                                                  onChange={(e) => {
                                                    const newFilters = [
                                                      ...config.network.incoming
                                                        .httpFilter,
                                                    ];
                                                    const headerIndex =
                                                      newFilters.findIndex(
                                                        (f, i) =>
                                                          f.type === "header" &&
                                                          i === index
                                                      );
                                                    if (headerIndex !== -1) {
                                                      newFilters[headerIndex] = {
                                                        ...newFilters[
                                                        headerIndex
                                                        ],
                                                        value: e.target.value,
                                                      };
                                                      setConfig({
                                                        ...config,
                                                        network: {
                                                          ...config.network,
                                                          incoming: {
                                                            ...config.network
                                                              .incoming,
                                                            httpFilter:
                                                              newFilters,
                                                          },
                                                        },
                                                      });
                                                    }
                                                  }}
                                                  className="flex-1"
                                                />
                                                <Select
                                                  value={
                                                    filter.matchType || "exact"
                                                  }
                                                  onValueChange={(
                                                    value: "exact" | "regex"
                                                  ) => {
                                                    const newFilters = [
                                                      ...config.network.incoming
                                                        .httpFilter,
                                                    ];
                                                    const headerIndex =
                                                      newFilters.findIndex(
                                                        (f, i) =>
                                                          f.type === "header" &&
                                                          i === index
                                                      );
                                                    if (headerIndex !== -1) {
                                                      newFilters[headerIndex] = {
                                                        ...newFilters[
                                                        headerIndex
                                                        ],
                                                        matchType: value,
                                                      };
                                                      setConfig({
                                                        ...config,
                                                        network: {
                                                          ...config.network,
                                                          incoming: {
                                                            ...config.network
                                                              .incoming,
                                                            httpFilter:
                                                              newFilters,
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
                                                    const newFilters =
                                                      config.network.incoming.httpFilter.filter(
                                                        (f, i) =>
                                                          !(
                                                            f.type === "header" &&
                                                            i === index
                                                          )
                                                      );
                                                    setConfig({
                                                      ...config,
                                                      network: {
                                                        ...config.network,
                                                        incoming: {
                                                          ...config.network
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
                                </div>

                                {/* Path Filtering */}
                                <div className="space-y-2">
                                  <div className="flex items-center justify-between">
                                    <Label className="font-medium">
                                      Path Filters
                                    </Label>
                                    <Button
                                      type="button"
                                      variant="outline"
                                      size="sm"
                                      onClick={() =>
                                        setConfig({
                                          ...config,
                                          network: {
                                            ...config.network,
                                            incoming: {
                                              ...config.network.incoming,
                                              httpFilter: [
                                                ...config.network.incoming
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

                                  {config.network.incoming.httpFilter.filter(
                                    (f) => f.type === "path"
                                  ).length > 0 && (
                                      <div className="space-y-2">
                                        {config.network.incoming.httpFilter
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
                                                    ...config.network.incoming
                                                      .httpFilter,
                                                  ];
                                                  const pathIndex =
                                                    newFilters.findIndex(
                                                      (f, i) =>
                                                        f.type === "path" &&
                                                        i === index
                                                    );
                                                  if (pathIndex !== -1) {
                                                    newFilters[pathIndex] = {
                                                      ...newFilters[pathIndex],
                                                      value: e.target.value,
                                                    };
                                                    setConfig({
                                                      ...config,
                                                      network: {
                                                        ...config.network,
                                                        incoming: {
                                                          ...config.network
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
                                                  const newFilters =
                                                    config.network.incoming.httpFilter.filter(
                                                      (f, i) =>
                                                        !(
                                                          f.type === "path" &&
                                                          i === index
                                                        )
                                                    );
                                                  setConfig({
                                                    ...config,
                                                    network: {
                                                      ...config.network,
                                                      incoming: {
                                                        ...config.network
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
                                {config.network.incoming.httpFilter.length >
                                  1 && (
                                    <>
                                      <Separator />
                                      <div className="space-y-2">
                                        <Label className="font-medium">
                                          Filter Logic
                                        </Label>
                                        <RadioGroup
                                          value={
                                            config.network.incoming.filterOperator
                                          }
                                          onValueChange={(value: "AND" | "OR") =>
                                            setConfig({
                                              ...config,
                                              network: {
                                                ...config.network,
                                                incoming: {
                                                  ...config.network.incoming,
                                                  filterOperator: value,
                                                },
                                              },
                                            })
                                          }
                                        >
                                          <div className="flex items-center space-x-2">
                                            <RadioGroupItem
                                              value="AND"
                                              id="and"
                                            />
                                            <Label
                                              htmlFor="and"
                                              className="text-sm"
                                            >
                                              <strong>All</strong> - Match all
                                              specified filters
                                            </Label>
                                          </div>
                                          <div className="flex items-center space-x-2">
                                            <RadioGroupItem value="OR" id="or" />
                                            <Label
                                              htmlFor="or"
                                              className="text-sm"
                                            >
                                              <strong>Any</strong> - Match any
                                              specified filter
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
                                  Click a port to enable HTTP filtering.
                                  Deselect any ports that are not HTTP or gRPC.
                                  The following ports were detected in your
                                  target, with some already preselected by
                                  default.
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
                                      const isSelected =
                                        config.network.incoming.ports.some(
                                          (p) => p.remote === port
                                        );
                                      const togglePort = () => {
                                        if (isSelected) {
                                          // Remove port
                                          const newPorts =
                                            config.network.incoming.ports.filter(
                                              (p) => p.remote !== port
                                            );
                                          setConfig({
                                            ...config,
                                            network: {
                                              ...config.network,
                                              incoming: {
                                                ...config.network.incoming,
                                                ports: newPorts,
                                              },
                                            },
                                          });
                                        } else {
                                          // Add port
                                          setConfig({
                                            ...config,
                                            network: {
                                              ...config.network,
                                              incoming: {
                                                ...config.network.incoming,
                                                ports: [
                                                  ...config.network.incoming
                                                    .ports,
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
                                          variant={
                                            isSelected ? "default" : "outline"
                                          }
                                          size="sm"
                                          className={`rounded-full px-4 py-2 font-mono transition-all ${isSelected
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
                                {config.network.incoming.ports.length > 0 && (
                                  <div className="space-y-3">
                                    <h4 className="text-base font-semibold">
                                      Selected Ports
                                    </h4>
                                    <p className="text-sm text-muted-foreground mb-3">
                                      Only map ports that run on different ports
                                      locally.
                                    </p>
                                    <div className="space-y-3">
                                      {config.network.incoming.ports.map(
                                        (portConfig, index) => (
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
                                                  portConfig.local !==
                                                  portConfig.remote
                                                }
                                                onCheckedChange={(checked) => {
                                                  const newPorts = [
                                                    ...config.network.incoming
                                                      .ports,
                                                  ];
                                                  newPorts[index] = {
                                                    ...newPorts[index],
                                                    local: checked
                                                      ? ""
                                                      : portConfig.remote,
                                                  };
                                                  setConfig({
                                                    ...config,
                                                    network: {
                                                      ...config.network,
                                                      incoming: {
                                                        ...config.network
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
                                                Local port is different than
                                                remote
                                              </Label>
                                            </div>

                                            {portConfig.local !==
                                              portConfig.remote && (
                                                <div className="flex items-center gap-3">
                                                  <div className="flex-1">
                                                    <Label className="text-xs text-muted-foreground">
                                                      Local Port
                                                    </Label>
                                                    <Input
                                                      className="font-mono"
                                                      placeholder={
                                                        portConfig.remote
                                                      }
                                                      value={portConfig.local}
                                                      onChange={(e) => {
                                                        const newPorts = [
                                                          ...config.network
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
                                                            ...config.network,
                                                            incoming: {
                                                              ...config.network
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
                                        )
                                      )}
                                    </div>
                                  </div>
                                )}
                              </div>
                            </div>
                          </div>
                        )}

                        {(selectedBoilerplate === "mirror" ||
                          selectedBoilerplate === "replace") && (
                            <div className="space-y-4">
                              {/* Simplified Port Configuration */}
                              <div>
                                <h3 className="text-base font-semibold mb-1">
                                  Port Configuration
                                </h3>
                                <p className="text-xs text-muted-foreground mb-3">
                                  Click ports to select them for mirroring
                                </p>
                              </div>

                              <div className="space-y-4">
                                {/* Detected Ports */}
                                <div>
                                  <h4 className="text-base font-semibold">
                                    Detected Ports
                                  </h4>
                                  <p className="text-sm text-muted-foreground mb-3">
                                    Click on a port to create a mapping.
                                  </p>
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
                                      const isSelected =
                                        config.network.incoming.ports.some(
                                          (p) => p.remote === port
                                        );
                                      const togglePort = () => {
                                        if (isSelected) {
                                          // Remove port
                                          const newPorts =
                                            config.network.incoming.ports.filter(
                                              (p) => p.remote !== port
                                            );
                                          setConfig({
                                            ...config,
                                            network: {
                                              ...config.network,
                                              incoming: {
                                                ...config.network.incoming,
                                                ports: newPorts,
                                              },
                                            },
                                          });
                                        } else {
                                          // Add port
                                          setConfig({
                                            ...config,
                                            network: {
                                              ...config.network,
                                              incoming: {
                                                ...config.network.incoming,
                                                ports: [
                                                  ...config.network.incoming
                                                    .ports,
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
                                          variant={
                                            isSelected ? "default" : "outline"
                                          }
                                          size="sm"
                                          className={`rounded-full px-4 py-2 font-mono transition-all ${isSelected
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
                                {config.network.incoming.ports.length > 0 && (
                                  <div className="space-y-3">
                                    <h4 className="text-base font-semibold">
                                      Port Mappings
                                    </h4>
                                    <div className="space-y-3">
                                      <div className="grid grid-cols-2 gap-4 text-sm font-medium text-muted-foreground">
                                        <div>Local Port</div>
                                        <div>Remote Port</div>
                                      </div>
                                      {config.network.incoming.ports.map(
                                        (portConfig, index) => (
                                          <div
                                            key={portConfig.remote}
                                            className="flex items-center gap-3"
                                          >
                                            <div className="flex-1">
                                              <Input
                                                className="font-mono"
                                                placeholder={portConfig.remote}
                                                value={portConfig.local}
                                                onChange={(e) => {
                                                  const newPorts = [
                                                    ...config.network.incoming
                                                      .ports,
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
                                                      ...config.network,
                                                      incoming: {
                                                        ...config.network
                                                          .incoming,
                                                        ports: newPorts,
                                                      },
                                                    },
                                                  });
                                                }}
                                              />
                                            </div>

                                            <ArrowRight className="h-4 w-4 text-muted-foreground flex-shrink-0" />

                                            <div className="flex-1">
                                              <Input
                                                className="font-mono"
                                                value={portConfig.remote}
                                                readOnly
                                              />
                                            </div>

                                            <Button
                                              variant="ghost"
                                              size="sm"
                                              className="h-8 w-8 p-0 text-red-500 hover:text-red-600 hover:bg-red-50"
                                              onClick={() => {
                                                const newPorts =
                                                  config.network.incoming.ports.filter(
                                                    (p) =>
                                                      p.remote !==
                                                      portConfig.remote
                                                  );
                                                setConfig({
                                                  ...config,
                                                  network: {
                                                    ...config.network,
                                                    incoming: {
                                                      ...config.network.incoming,
                                                      ports: newPorts,
                                                    },
                                                  },
                                                });
                                              }}
                                            >
                                              <Trash2 className="h-4 w-4" />
                                            </Button>
                                          </div>
                                        )
                                      )}
                                    </div>
                                  </div>
                                )}
                              </div>
                            </div>
                          )}
                      </div>
                    </CardContent>
                  </Card>
                </TabsContent>

                {/* Export Tab */}
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
                            value={editableJson || generateConfigJson(config)}
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
                          {copied ? (
                            <Check className="h-4 w-4 mr-2" />
                          ) : (
                            <Copy className="h-4 w-4 mr-2" />
                          )}
                          Copy to Clipboard
                        </Button>

                        <DownloadButton
                          json={editableJson || generateConfigJson(config)}
                          filename={config.name || "mirrord-config"}
                        />

                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => {
                            setEditableJson(generateConfigJson(config));
                            setJsonError("");
                          }}
                        >
                          Reset to Generated
                        </Button>
                      </div>

                      <Separator />

                      <div className="space-y-3">
                        <h4 className="font-medium">
                          How to use your configuration:
                        </h4>
                        <div className="space-y-2 text-sm text-muted-foreground">
                          <p>
                            <strong>CLI:</strong> Use the{" "}
                            <code className="px-1 py-0.5 bg-muted rounded text-xs">
                              -f &lt;CONFIG_PATH&gt;
                            </code>{" "}
                            flag
                          </p>
                          <p>
                            <strong>
                              VSCode Extension or JetBrains plugin:
                            </strong>{" "}
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
          )}
        </ScrollArea>

        {/* Fixed Footer with Navigation */}
        <WizardFooter
          isReturning={isReturning}
          onboardingStep={onboardingStep}
          currentTab={currentTab}
          configTarget={config.target}
          jsonError={jsonError}
          editableJson={editableJson}
          setOnboardingStep={setOnboardingStep}
          setCurrentTab={setCurrentTab}
          generateConfigJson={generateConfigJson}
          updateConfigFromJson={updateConfigFromJson}
          setConfig={setConfig}
          setJsonError={setJsonError}
          config={config}
          onSave={onSave}
          onClose={onClose}
        />
      </DialogContent>
    </Dialog>
  );
}
