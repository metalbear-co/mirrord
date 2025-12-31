import { ArrowRight, Trash2 } from "lucide-react";
import { Button } from "../ui/button";
import { Input } from "../ui/input";
import {
  addRemoveOrUpdateMapping,
  getLocalPort,
  readBoilerplateType,
  readCurrentPorts,
  removePortandMapping,
} from "../JsonUtils";
import { useContext, useState } from "react";
import { ConfigDataContext } from "../UserDataContext";
import { useToast } from "../ui/use-toast";

export const PortMappingEntry = ({
  remotePort,
  detectedPort,
  setPortConflicts,
}: {
  remotePort: number;
  detectedPort: boolean;
  setPortConflicts: (value: boolean) => void;
}) => {
  const { config, setConfig } = useContext(ConfigDataContext)!;
  const [inputContents, setInputContents] = useState<number>(
    getLocalPort(remotePort, config),
  );
  const [outlineConflict, setOutlineConflict] = useState<boolean>(false);

  // remote ports are known to be unique due to the ui structure
  // manually enforce that local ports are unique
  const { toast, dismiss } = useToast();
  const localPortConflict = async () => {
    setOutlineConflict(true);
    setPortConflicts(true);
    // toast does not take prop for stickiness, so set it to stay open for one year
    toast({
      title: "Local Port Conflict!",
      description:
        "Multiple port mappings have the same local port. Local ports should be unique.",
      duration: 31536000000,
    });
  };
  const resolveConflict = () => {
    setOutlineConflict(false);
    setPortConflicts(false);
    dismiss();
  };

  return (
    // remotePort is guarateed to be unique for each PortMapping
    <div className="border rounded-lg p-3 space-y-3">
      <div className="flex items-center gap-3">
        <div className="flex-1 font-mono">
          <p className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-base ring-offset-background md:text-sm">
            {remotePort}
          </p>
        </div>

        <ArrowRight className="h-4 w-4 text-muted-foreground flex-shrink-0" />

        <div className="flex-1">
          <Input
            type="text"
            pattern="[0-9]*"
            className={
              outlineConflict
                ? "font-mono ring-2 ring-red-500 ring-offset-2 focus-visible:ring-red-500"
                : "font-mono"
            }
            value={inputContents}
            onChange={(event) => {
              // remove all whitespace from the input
              const newValue = +event.target.value.replace(/\s/g, "");
              if (!isNaN(newValue)) {
                if (
                  readCurrentPorts(config).filter(
                    (remote) => getLocalPort(remote, config) === newValue,
                  ).length > 0
                ) {
                  localPortConflict();
                } else {
                  resolveConflict();
                }
                const newConfig = addRemoveOrUpdateMapping(
                  remotePort,
                  newValue,
                  config,
                );
                setConfig(newConfig);

                setInputContents(newValue);
              }
            }}
          />
        </div>

        {/* Delete button */}
        {!(readBoilerplateType(config) === "replace" && detectedPort) && (
          <Button
            variant="ghost"
            size="sm"
            className="h-8 w-8 p-0 text-red-500 hover:text-red-600 hover:bg-red-50"
            onClick={() => {
              const newConfig = removePortandMapping(remotePort, config);
              setConfig(newConfig);
              resolveConflict();
            }}
          >
            <Trash2 className="h-4 w-4" />
          </Button>
        )}
      </div>
    </div>
  );
};

export default PortMappingEntry;
