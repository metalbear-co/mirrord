import { ArrowRight, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  addRemoveOrUpdateMapping,
  getLocalPort,
  regexificationRay,
  removePortandMapping,
  removeSingleFilter,
  updateSingleFilter,
} from "../JsonUtils";
import { useContext, useState } from "react";
import { ConfigDataContext } from "../UserDataContext";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../ui/select";

export const HttpFilter = ({
  initValue,
  inputType,
}: {
  initValue: string;
  inputType: "header" | "path";
}) => {
  const { config, setConfig } = useContext(ConfigDataContext);

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        <Input
          placeholder="e.g., x-mirrord-test: true"
          value={initValue}
          readOnly={true}
          className="flex-1"
        />

        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => {
            const newConfig = removeSingleFilter(
              { value: initValue, type: inputType },
              config
            );
            setConfig(newConfig);
          }}
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>
    </div>
  );
};

export default HttpFilter;
