import { useToast } from "@/hooks/use-toast";
import { useState, useContext } from "react";
import { Copy, Save, ChevronLeft, ChevronRight } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  CardDescription,
} from "@/components/ui/card";
import { Label } from "@/components/ui/label";

import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Separator } from "@/components/ui/separator";
import { Textarea } from "@/components/ui/textarea";
import DownloadButton from "../DownloadButton";
import {
  getConfigString,
  readCurrentTargetDetails,
  readIncoming,
} from "../JsonUtils";
import { ConfigDataContext, DefaultConfig } from "../UserDataContext";
import NetworkTab from "./NetworkTab";
import TargetTab from "./TargetTab";

const ConfigTabs = () => {
  const { config } = useContext(ConfigDataContext);
  const [currentTab, setCurrentTab] = useState<string>("target");
  const [savedIncoming, setSavedIncoming] = useState<any>(readIncoming(config));
  const [portConflicts, setPortConflicts] = useState<boolean>(false);
  const [targetPorts, setTargetPorts] = useState<number[]>([]);

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
          setCurrentTab(value);
        }}
        className="w-full"
      >
        <TabsList className="grid w-full grid-cols-3 h-10">
          <TabsTrigger
            value="target"
            className="text-sm"
            disabled={portConflicts}
          >
            Target
          </TabsTrigger>
          <TabsTrigger
            value="network"
            className="text-sm"
            disabled={targetNotSelected()}
          >
            Network
          </TabsTrigger>
          <TabsTrigger
            value="export"
            className="text-sm"
            disabled={targetNotSelected() || portConflicts}
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
        <TargetTab setTargetPorts={setTargetPorts} />

        <NetworkTab
          savedIncoming={savedIncoming}
          targetPorts={targetPorts}
          setSavedIncoming={setSavedIncoming}
          setPortConflicts={setPortConflicts}
        />

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
                disabled={targetNotSelected() || portConflicts}
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
