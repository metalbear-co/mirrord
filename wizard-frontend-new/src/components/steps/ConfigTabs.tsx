import { useToast } from "../../hooks/use-toast";
import { useState, useContext, useEffect } from "react";
import {
  Copy,
  FileJson,
  Download,
  Check,
  Terminal,
  Code2,
  FolderOpen,
} from "lucide-react";
import { Button, Separator } from "@metalbear/ui";
import {
  getConfigString,
  readCurrentTargetDetails,
  readIncoming,
} from "../JsonUtils";
import { ConfigDataContext } from "../UserDataContext";
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

  const [copied, setCopied] = useState(false);

  const copyToClipboard = async () => {
    const jsonToCopy = getConfigString(config);
    await navigator.clipboard.writeText(jsonToCopy);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
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
          <div className="space-y-5">
            <div className="flex items-center gap-3 pb-4 border-b border-[var(--border)]">
              <div className="w-10 h-10 rounded-lg bg-primary/10 flex items-center justify-center">
                <FileJson className="h-5 w-5 text-primary" />
              </div>
              <div>
                <h3 className="text-lg font-semibold">Export Configuration</h3>
                <p className="text-sm text-[var(--muted-foreground)]">
                  Review and export your mirrord.json configuration
                </p>
              </div>
            </div>

            {/* Code block */}
            <div className="rounded-xl border border-[var(--border)] overflow-hidden">
              <div className="flex items-center justify-between px-4 py-2.5 bg-[var(--muted)]/50 border-b border-[var(--border)]">
                <div className="flex items-center gap-2 text-xs text-[var(--muted-foreground)]">
                  <FileJson className="h-3.5 w-3.5" />
                  mirrord.json
                </div>
                <div className="flex items-center gap-1.5">
                  <button
                    onClick={copyToClipboard}
                    className="flex items-center gap-1.5 text-xs px-2.5 py-1 rounded-md text-[var(--muted-foreground)] hover:text-[var(--foreground)] hover:bg-[var(--muted)] transition-colors"
                  >
                    {copied ? (
                      <>
                        <Check className="h-3.5 w-3.5 text-green-500" />
                        Copied
                      </>
                    ) : (
                      <>
                        <Copy className="h-3.5 w-3.5" />
                        Copy
                      </>
                    )}
                  </button>
                </div>
              </div>
              <pre className="p-4 text-sm font-mono leading-relaxed overflow-x-auto bg-[var(--card)] max-h-[240px] overflow-y-auto">
                <code>{getConfigString(config)}</code>
              </pre>
            </div>

            <div className="flex flex-wrap gap-2">
              <Button
                variant="outline"
                size="sm"
                onClick={copyToClipboard}
                className="gap-2"
              >
                {copied ? (
                  <Check className="h-4 w-4 text-green-500" />
                ) : (
                  <Copy className="h-4 w-4" />
                )}
                {copied ? "Copied!" : "Copy to Clipboard"}
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={downloadConfig}
                className="gap-2"
              >
                <Download className="h-4 w-4" />
                Download File
              </Button>
            </div>

            <Separator />

            <div className="space-y-3">
              <h4 className="text-sm font-semibold text-[var(--foreground)]">
                How to use your configuration
              </h4>
              <div className="grid gap-2">
                <div className="flex items-start gap-3 p-3 rounded-lg bg-[var(--muted)]/30 border border-[var(--border)]">
                  <Terminal className="h-4 w-4 text-primary mt-0.5 flex-shrink-0" />
                  <div className="text-sm">
                    <span className="font-medium text-[var(--foreground)]">
                      CLI:{" "}
                    </span>
                    <span className="text-[var(--muted-foreground)]">
                      Use{" "}
                      <code className="px-1.5 py-0.5 bg-[var(--muted)] rounded text-xs font-semibold">
                        mirrord exec -f mirrord.json
                      </code>
                    </span>
                  </div>
                </div>
                <div className="flex items-start gap-3 p-3 rounded-lg bg-[var(--muted)]/30 border border-[var(--border)]">
                  <Code2 className="h-4 w-4 text-primary mt-0.5 flex-shrink-0" />
                  <div className="text-sm">
                    <span className="font-medium text-[var(--foreground)]">
                      IDE:{" "}
                    </span>
                    <span className="text-[var(--muted-foreground)]">
                      Save as{" "}
                      <code className="px-1.5 py-0.5 bg-[var(--muted)] rounded text-xs font-semibold">
                        .mirrord/mirrord.json
                      </code>
                    </span>
                  </div>
                </div>
                <a
                  href="https://mirrord.dev/docs/reference/configuration/"
                  target="_blank"
                  rel="noopener noreferrer"
                  className="flex items-center gap-3 p-3 rounded-lg hover:bg-primary/5 border border-transparent hover:border-primary/10 transition-all text-sm text-primary"
                >
                  <FolderOpen className="h-4 w-4 flex-shrink-0" />
                  View full configuration reference
                </a>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
};

export default ConfigTabs;
