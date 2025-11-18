import { ArrowRight, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  addRemoveOrUpdateMapping,
  getLocalPort,
  readCurrentPorts,
  removePortandMapping,
} from "../JsonUtils";
import { useContext, useState } from "react";
import { ConfigDataContext } from "../UserDataContext";
import { useToast } from "../ui/use-toast";

export const PortMapping = ({ remotePort }: { remotePort: number }) => {
  const { config, setConfig } = useContext(ConfigDataContext);
  const [inputContents, setInputContents] = useState<number>(
    getLocalPort(remotePort, config)
  );
  const [outlineConflict, setOutlineConflict] = useState<boolean>(false);

  // remote ports are known to be unique due to the ui structure
  // manually enforce that local ports are unique
  const { toast, dismiss } = useToast();
  const localPortConflict = async () => {
    setOutlineConflict(true);
    toast({
      title: "Local Port Conflict!",
      description:
        "Multiple port mappings have the same local port. Local ports should be unique.",
    });
  };
  const resolveConflict = () => {
    setOutlineConflict(false);
    dismiss();
  }

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
              if (!isNaN(+event.target.value)) {
                if (
                  readCurrentPorts(config).filter(
                    (remote) =>
                      getLocalPort(remote, config) === +event.target.value
                  ).length > 0
                ) {
                  localPortConflict();
                } else {
                  resolveConflict();
                }
                const newConfig = addRemoveOrUpdateMapping(
                  remotePort,
                  +event.target.value,
                  config
                );
                setConfig(newConfig);

                setInputContents(+event.target.value);
              }
            }}
          />
        </div>

        {/* Delete button */}
        <Button
          variant="ghost"
          size="sm"
          className="h-8 w-8 p-0 text-red-500 hover:text-red-600 hover:bg-red-50"
          onClick={() => {
            const newConfig = removePortandMapping(remotePort, config);
            setConfig(newConfig);
          }}
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
};

export default PortMapping;
