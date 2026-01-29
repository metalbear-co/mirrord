import { useToast } from "../../hooks/use-toast";
import { useState, useContext } from "react";
import { Copy, Save, ChevronLeft, ChevronRight, Download } from "lucide-react";
import {
  Button,
  Label,
  Separator,
  Textarea,
} from "@metalbear/ui";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  CardDescription,
} from "@metalbear/ui";
import {
  getConfigString,
  readCurrentTargetDetails,
  readIncoming,
} from "../JsonUtils";
import { ConfigDataContext, DefaultConfig } from "../UserDataContext";
import type { ToggleableConfigFor_IncomingFileConfig } from "../../mirrord-schema";
import NetworkTab from "./NetworkTab";
import TargetTab from "./TargetTab";

type CurrentTabId = "target" | "network" | "export";

const ConfigTabs = () => {
  const { config } = useContext(ConfigDataContext)!;
  const [currentTab, setCurrentTab] = useState<CurrentTabId>("target");
  const [savedIncoming, setSavedIncoming] =
    useState<ToggleableConfigFor_IncomingFileConfig>(readIncoming(config));
  const [portConflicts, setPortConflicts] = useState<boolean>(false);
  const [targetPorts, setTargetPorts] = useState<number[]>([]);

  const { toast } = useToast();

  const copyToClipboard = async () => {
    const jsonToCopy = getConfigString(config);
    await navigator.clipboard.writeText(jsonToCopy);
    toast({
      title: "Copied to clipboard",
      description: "Configuration JSON has been copied to your clipboard.",
    });
  };

  const downloadConfig = () => {
    const jsonContent = getConfigString(config);
    const blob = new Blob([jsonContent], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "mirrord.json";
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
    toast({
      title: "Downloaded",
      description: "Configuration saved as mirrord.json",
    });
  };

  const nextTab = () => {
    if (currentTab === "target") setCurrentTab("network");
    else if (currentTab === "network") setCurrentTab("export");
  };

  const prevTab = () => {
    if (currentTab === "export") setCurrentTab("network");
    else if (currentTab === "network") setCurrentTab("target");
  };

  const targetNotSelected = (): boolean => {
    return typeof readCurrentTargetDetails(config).name !== "string";
  };

  return (
    <div className="space-y-6">
      {/* Tab Navigation */}
      <div className="flex border-b border-[var(--border)]">
        <button
          onClick={() => setCurrentTab("target")}
          className={`px-4 py-2 text-sm font-medium border-b-2 transition-colors ${
            currentTab === "target"
              ? "border-primary text-primary"
              : "border-transparent text-[var(--muted-foreground)] hover:text-[var(--foreground)]"
          }`}
          disabled={portConflicts}
        >
          Target
        </button>
        <button
          onClick={() => !targetNotSelected() && setCurrentTab("network")}
          className={`px-4 py-2 text-sm font-medium border-b-2 transition-colors ${
            currentTab === "network"
              ? "border-primary text-primary"
              : "border-transparent text-[var(--muted-foreground)] hover:text-[var(--foreground)]"
          } ${targetNotSelected() ? "opacity-50 cursor-not-allowed" : ""}`}
          disabled={targetNotSelected()}
        >
          Network
        </button>
        <button
          onClick={() => !targetNotSelected() && !portConflicts && setCurrentTab("export")}
          className={`px-4 py-2 text-sm font-medium border-b-2 transition-colors ${
            currentTab === "export"
              ? "border-primary text-primary"
              : "border-transparent text-[var(--muted-foreground)] hover:text-[var(--foreground)]"
          } ${targetNotSelected() || portConflicts ? "opacity-50 cursor-not-allowed" : ""}`}
          disabled={targetNotSelected() || portConflicts}
        >
          Export
        </button>
      </div>

      {/* Tab Content */}
      <div className="mt-6">
        {currentTab === "target" && (
          <TargetTab setTargetPorts={setTargetPorts} />
        )}

        {currentTab === "network" && (
          <NetworkTab
            savedIncoming={savedIncoming}
            targetPorts={targetPorts}
            setSavedIncoming={setSavedIncoming}
            setPortConflicts={setPortConflicts}
          />
        )}

        {currentTab === "export" && (
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2 text-lg">
                <Save className="h-5 w-5" />
                Export Configuration
              </CardTitle>
              <CardDescription>
                Review and export your mirrord.json configuration
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="space-y-2">
                <Label htmlFor="json-editor">Configuration JSON</Label>
                <Textarea
                  id="json-editor"
                  className="font-code text-sm min-h-[200px] resize-none"
                  value={getConfigString(config)}
                  readOnly
                  placeholder={getConfigString(DefaultConfig)}
                />
              </div>

              <div className="flex flex-wrap gap-2">
                <Button variant="outline" size="sm" onClick={copyToClipboard}>
                  <Copy className="h-4 w-4 mr-2" />
                  Copy to Clipboard
                </Button>
                <Button variant="outline" size="sm" onClick={downloadConfig}>
                  <Download className="h-4 w-4 mr-2" />
                  Download File
                </Button>
              </div>

              <Separator />

              <div className="space-y-3">
                <h4 className="font-medium text-[var(--foreground)]">
                  How to use your configuration:
                </h4>
                <div className="space-y-2 text-sm text-[var(--muted-foreground)]">
                  <p>
                    <strong>CLI:</strong> Use the{" "}
                    <code className="px-1 py-0.5 bg-[var(--muted)] rounded text-xs">
                      -f &lt;CONFIG_PATH&gt;
                    </code>{" "}
                    flag
                  </p>
                  <p>
                    <strong>VSCode / JetBrains:</strong> Create a{" "}
                    <code className="px-1 py-0.5 bg-[var(--muted)] rounded text-xs">
                      .mirrord/mirrord.json
                    </code>{" "}
                    file
                  </p>
                  <p>
                    See the{" "}
                    <a
                      href="https://mirrord.dev/docs/reference/configuration/"
                      target="_blank"
                      rel="noopener noreferrer"
                      className="text-primary hover:underline"
                    >
                      configuration documentation
                    </a>{" "}
                    for more details.
                  </p>
                </div>
              </div>
            </CardContent>
          </Card>
        )}
      </div>

      {/* Navigation buttons */}
      <div className="flex items-center justify-between pt-4 border-t border-[var(--border)]">
        <div>
          {currentTab !== "target" && (
            <Button variant="outline" onClick={prevTab}>
              <ChevronLeft className="h-4 w-4 mr-2" />
              Back
            </Button>
          )}
        </div>
        <div>
          {currentTab !== "export" && (
            <Button
              onClick={nextTab}
              disabled={targetNotSelected() || portConflicts}
            >
              Next
              <ChevronRight className="h-4 w-4 ml-2" />
            </Button>
          )}
        </div>
      </div>
    </div>
  );
};

export default ConfigTabs;
