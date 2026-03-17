import { useToast } from "../../hooks/use-toast";
import { useState, useContext, useEffect } from "react";
import { Copy, Save, Download } from "lucide-react";
import { Button, Label, Separator, Textarea } from "@metalbear/ui";
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

interface ConfigTabsProps {
  currentTab: CurrentTabId;
  onTabChange: (tab: CurrentTabId) => void;
  onCanAdvanceChange: (canAdvance: boolean) => void;
}

const ConfigTabs = ({
  currentTab,
  onTabChange,
  onCanAdvanceChange,
}: ConfigTabsProps) => {
  const { config } = useContext(ConfigDataContext)!;
  const [savedIncoming, setSavedIncoming] =
    useState<ToggleableConfigFor_IncomingFileConfig>(readIncoming(config));
  const [portConflicts, setPortConflicts] = useState<boolean>(false);
  const [targetPorts, setTargetPorts] = useState<number[]>([]);

  const { toast } = useToast();

  const targetNotSelected = (): boolean => {
    return typeof readCurrentTargetDetails(config).name !== "string";
  };

  // Notify parent about whether we can advance
  useEffect(() => {
    const canAdvance = !targetNotSelected() && !portConflicts;
    onCanAdvanceChange(canAdvance);
  }, [config, portConflicts, onCanAdvanceChange]);

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

  return (
    <div className="space-y-6">
      {/* Tab Navigation */}
      <div className="flex border-b border-[var(--border)]">
        <button
          onClick={() => onTabChange("target")}
          className={`px-4 py-2.5 text-sm font-medium border-b-2 -mb-px transition-colors ${
            currentTab === "target"
              ? "border-primary text-primary"
              : "border-transparent text-[var(--muted-foreground)] hover:text-[var(--foreground)]"
          }`}
        >
          Target
        </button>
        <button
          onClick={() => !targetNotSelected() && onTabChange("network")}
          className={`px-4 py-2.5 text-sm font-medium border-b-2 -mb-px transition-colors ${
            currentTab === "network"
              ? "border-primary text-primary"
              : "border-transparent text-[var(--muted-foreground)] hover:text-[var(--foreground)]"
          } ${targetNotSelected() ? "opacity-50 cursor-not-allowed" : ""}`}
        >
          Network
        </button>
        <button
          onClick={() =>
            !targetNotSelected() && !portConflicts && onTabChange("export")
          }
          className={`px-4 py-2.5 text-sm font-medium border-b-2 -mb-px transition-colors ${
            currentTab === "export"
              ? "border-primary text-primary"
              : "border-transparent text-[var(--muted-foreground)] hover:text-[var(--foreground)]"
          } ${targetNotSelected() || portConflicts ? "opacity-50 cursor-not-allowed" : ""}`}
        >
          Export
        </button>
      </div>

      {/* Tab Content */}
      <div key={currentTab} className="animate-tab-in">
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
          <div className="space-y-6">
            <div className="flex items-center gap-3 pb-4 border-b border-[var(--border)]">
              <div className="w-10 h-10 rounded-lg bg-primary/10 flex items-center justify-center">
                <Save className="h-5 w-5 text-primary" />
              </div>
              <div>
                <h3 className="text-lg font-semibold">Export Configuration</h3>
                <p className="text-sm text-[var(--muted-foreground)]">
                  Review and export your mirrord.json configuration
                </p>
              </div>
            </div>

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
          </div>
        )}
      </div>
    </div>
  );
};

export default ConfigTabs;
