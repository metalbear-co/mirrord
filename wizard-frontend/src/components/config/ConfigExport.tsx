import { useState } from "react";
import { Copy, Check, Download } from "lucide-react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { useToast } from "@/hooks/use-toast";
import type { FeatureConfig, ConfigData } from "@/types/config";


interface ConfigExportProps {
  config: ConfigData;
}

export function ConfigExport({ config }: ConfigExportProps) {
  const [copied, setCopied] = useState(false);
  const [editableJson, setEditableJson] = useState<string>("");
  const [jsonError, setJsonError] = useState<string>("");
  const { toast } = useToast();

  type FeatureConfig = {
    network?: {
      incoming?: {
        mode: "steal" | "mirror";
        http_filter?: Record<string, Array<Record<string, string>>>;
        ports?: Array<Record<string, string>>;
      };
      outgoing?: {
        filter: Record<string, string | Record<string, string>>;
      };
    };
    fs?: {
      mode: "read" | "write" | "local";
    };
    env?: {
      include?: string;
      exclude?: string;
      override?: string;
    };
  };

  const generateConfigJson = () => {
    const configObj: {
      target?: string;
      agent: {
        copy_target?: boolean;
        scaledown?: boolean;
      };
      feature: FeatureConfig;
    } = {
      target: config.target ? `${config.target}` : undefined,
      agent: {},
      feature: {}
    };

    if (config.agent.copyTarget) configObj.agent.copy_target = true;
    if (config.agent.scaledown) configObj.agent.scaledown = true;

    if (config.network.incoming.enabled) {
      configObj.feature.network = {
        incoming: {
          mode: config.network.incoming.mode
        }
      };

      if (config.network.incoming.httpFilter.length > 0) {
        configObj.feature.network.incoming.http_filter = {
          [config.network.incoming.filterOperator.toLowerCase()]: config.network.incoming.httpFilter.map(f => ({
            [f.type]: f.value
          }))
        };
      }

      if (config.network.incoming.ports.length > 0) {
        configObj.feature.network.incoming.ports = config.network.incoming.ports.map(p => ({
          [p.remote]: p.local
        }));
      }
    }

    if (config.network.outgoing.enabled) {
      if (!configObj.feature.network) configObj.feature.network = {};
      configObj.feature.network.outgoing = {
        filter: {
          [config.network.outgoing.filterTarget]: config.network.outgoing.protocol !== "both" ? {
            [config.network.outgoing.protocol]: config.network.outgoing.filter
          } : config.network.outgoing.filter
        }
      };
    }

    if (config.fileSystem.enabled) {
      configObj.feature.fs = {
        mode: config.fileSystem.mode
      };
    }

    if (config.environment.enabled) {
      configObj.feature.env = {};
      if (config.environment.include) configObj.feature.env.include = config.environment.include;
      if (config.environment.exclude) configObj.feature.env.exclude = config.environment.exclude;
      if (config.environment.override) configObj.feature.env.override = config.environment.override;
    }

    return JSON.stringify(configObj, null, 2);
  };

  const validateJson = (jsonString: string) => {
    try {
      JSON.parse(jsonString);
      setJsonError("");
      return true;
    } catch (error) {
      setJsonError(`Invalid JSON: ${error instanceof Error ? error.message : 'Unknown error'}`);
      return false;
    }
  };

  const copyToClipboard = async () => {
    const jsonToCopy = editableJson || generateConfigJson();
    await navigator.clipboard.writeText(jsonToCopy);
    setCopied(true);
    toast({
      title: "Copied to clipboard",
      description: "Configuration JSON has been copied to your clipboard."
    });
    setTimeout(() => setCopied(false), 2000);
  };

  const downloadJson = () => {
    const jsonToCopy = editableJson || generateConfigJson();
    const blob = new Blob([jsonToCopy], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `${config.name || 'mirrord-config'}.json`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
    toast({
      title: "Downloaded",
      description: "Configuration JSON has been downloaded."
    });
  };

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-base">Export Configuration</CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <Textarea
          placeholder="Generated JSON will appear here..."
          value={editableJson || generateConfigJson()}
          onChange={(e) => {
            setEditableJson(e.target.value);
            validateJson(e.target.value);
          }}
          className="font-mono text-xs min-h-[200px]"
        />
        
        {jsonError && (
          <p className="text-sm text-red-500">{jsonError}</p>
        )}
        
        <div className="flex gap-2">
          <Button variant="outline" size="sm" onClick={copyToClipboard}>
            {copied ? <Check className="h-4 w-4 mr-2" /> : <Copy className="h-4 w-4 mr-2" />}
            {copied ? "Copied!" : "Copy"}
          </Button>
          <Button variant="outline" size="sm" onClick={downloadJson}>
            <Download className="h-4 w-4 mr-2" />
            Download
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}