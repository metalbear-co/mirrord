import { useToast } from "@/hooks/use-toast";
import { useState, useContext } from "react";
import {
  Copy,
  Save,
  Server,
  AlertCircle,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
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
import { Separator } from "@/components/ui/separator";
import { Textarea } from "@/components/ui/textarea";
import DownloadButton from "../DownloadButton";
import {
  getConfigString,
  readCurrentTargetDetails,
  readIncoming,
  updateConfigTarget,
} from "../JsonUtils";
import { ConfigDataContext, DefaultConfig } from "../UserDataContext";
import NetworkTab from "./NetworkTab";

const ConfigTabs = () => {
  const { config, setConfig } = useContext(ConfigDataContext);
  const [currentTab, setCurrentTab] = useState<string>("target");
  const [namespace, setNamespace] = useState<string>("default");
  const [targetType, setTargetType] = useState<string>("");
  const [savedIncoming, setSavedIncoming] = useState<any>(readIncoming(config));

  // For copying to clipboard
  const { toast } = useToast();
  const copyToClipboard = async () => {
    const jsonToCopy = getConfigString(config);
    await navigator.clipboard.writeText(jsonToCopy);
    toast({
      title: "Copied to clipboard",
      description: "Configuration JSON has been copied to your clipboard.",
    });
  };

  const mockTargetTypes = [
    "Deployment",
    "Stateful Set",
    "Daemon Set",
    "Pod",
    "Cron Job",
    "Job",
  ];
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

  const [targetSearchText, setTargetSearchText] = useState<string>("");

  const nextTab = () => {
    if (currentTab === "target") setCurrentTab("network");
    else if (currentTab === "network") setCurrentTab("export");
    else setCurrentTab("export");
  };
  const prevTab = () => {
    if (currentTab === "export") setCurrentTab("network");
    else if (currentTab === "network") setCurrentTab("target");
    else setCurrentTab("target");
  };

  const targetNotSelected = (): boolean => {
    return typeof readCurrentTargetDetails(config).name !== "string";
  };

  return (
    <>
      {/* Tabs navigation above configuration options */}
      <Tabs
        value={currentTab}
        onValueChange={(value) => {
          // Only allow navigation if target selected
          if (targetNotSelected()) return;
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
            disabled={!config.target}
          >
            Network
          </TabsTrigger>
          <TabsTrigger
            value="export"
            className="text-sm"
            disabled={!config.target}
          >
            Export
          </TabsTrigger>
        </TabsList>
      </Tabs>
      {/* Main configuration options */}
      <Tabs
        value={currentTab}
        onValueChange={(value) => {
          // Only allow navigation to another tab if target selected
          if (targetNotSelected()) return;
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
                  value={namespace}
                  onValueChange={(value) => setNamespace(value)}
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
                  value={targetType}
                  onValueChange={(value) => setTargetType(value)}
                >
                  <SelectTrigger>
                    <SelectValue placeholder="Select resource type" />
                  </SelectTrigger>
                  <SelectContent>
                    {mockTargetTypes.map((ttype) => {
                      return <SelectItem value={ttype}>{ttype}</SelectItem>;
                    })}
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
                        {mockTargets
                          .filter((target) => {
                            return target.name.includes(targetSearchText);
                          })
                          .map((target) => (
                            <div
                              key={`${target.namespace}/${target.name}`}
                              className="flex items-center justify-between p-2 hover:bg-muted rounded-md cursor-pointer"
                              onClick={() => {
                                const targetValue = `${target.kind}/${target.name}`;
                                const updated = updateConfigTarget(
                                  config,
                                  targetValue,
                                  target.namespace
                                );
                                setConfig(updated);
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

        <NetworkTab savedIncoming={savedIncoming} setSavedIncoming={setSavedIncoming} />

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
                    value={getConfigString(config)}
                    readOnly={true}
                    placeholder={getConfigString(DefaultConfig)}
                  />
                </div>
              </div>

              <div className="flex flex-wrap gap-2">
                <Button variant="outline" size="sm" onClick={copyToClipboard}>
                  <Copy className="h-4 w-4 mr-2" />
                  Copy to Clipboard
                </Button>

                <DownloadButton
                  json={getConfigString(config)}
                  filename={"mirrord-config"}
                />
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
                      href="https://metalbear.com/mirrord/docs/config#basic-mirrord.json"
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
      <div>
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            {currentTab !== "target" && (
              <Button
                variant="outline"
                onClick={prevTab}
                className="flex items-center gap-2"
              >
                <ChevronLeft className="h-4 w-4" />
                Back
              </Button>
            )}
          </div>

          <div className="flex items-center gap-2">
            {currentTab !== "export" && (
              <Button
                onClick={nextTab}
                className="flex items-center gap-2"
                disabled={targetNotSelected()}
              >
                Next
                <ChevronRight className="h-4 w-4" />
              </Button>
            )}
          </div>
        </div>
      </div>
    </>
  );
};

export default ConfigTabs;
