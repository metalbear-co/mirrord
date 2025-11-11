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
  // id is guarateed to be unique
  const { config, setConfig } = useContext(ConfigDataContext);
  const [inputValue, setInputValue] = useState<string>(initValue);

  // filters are pulled from config, where they are always regex
  // if a user navigates away from the tab where a filter is set to exact match and then returns,
  // the filter will show as regex match with the equivalent regex.
  const [inputMatching, setInputMatching] = useState<"exact" | "regex">(
    "regex"
  );

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2">
        <Input
          placeholder="e.g., x-mirrord-test: true"
          value={inputValue}
          onChange={(event) => {
            if (event.target.value.length > 0) {
              // if inputMatching=exact, regexify the new AND old values before continuing
              const newValue =
                inputMatching === "exact"
                  ? regexificationRay(event.target.value)
                  : event.target.value;
              const oldValue =
                inputMatching === "exact"
                  ? regexificationRay(inputValue)
                  : inputValue;

              const newConfig = updateSingleFilter(
                { value: oldValue, type: inputType },
                { value: newValue, type: inputType },
                config
              );
              setConfig(newConfig);
            }
            setInputValue(event.target.value);
          }}
          className="flex-1"
        />

        {/* Selection between exact or regex matching */}
        <Select
          value={inputMatching}
          onValueChange={(value: "exact" | "regex") => {
            // exact -> regex = replace regexified inputValue in config with inputValue
            // regex -> exact = replace inputValue in config with regexified inputValue
            const newValue =
              value === "exact" ? regexificationRay(inputValue) : inputValue;
            const oldValue =
              inputMatching === "exact"
                ? regexificationRay(inputValue)
                : inputValue;

            const newConfig = updateSingleFilter(
              { value: oldValue, type: inputType },
              { value: newValue, type: inputType },
              config
            );
            setConfig(newConfig);

            setInputMatching(value);
          }}
        >
          <SelectTrigger className="w-40">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="exact">Exact</SelectItem>
            <SelectItem value="regex">Regex</SelectItem>
          </SelectContent>
        </Select>

        {/* render the regex equivalent when user does an exact match on a string */}
        {inputMatching === "exact" && (
          <div className="flex items-center gap-2">
            <ArrowRight className="h-4 w-4 text-muted-foreground flex-shrink-0" />

            <div className="flex-2 font-mono">
              <p className="flex h-10 w-full rounded-md border border-input px-3 py-2 text-muted-foreground ring-offset-background md:text-sm">
                {regexificationRay(inputValue)}
              </p>
            </div>
          </div>
        )}

        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => {
            const removedValue =
              inputMatching === "exact"
                ? regexificationRay(inputValue)
                : inputValue;
            const newConfig = removeSingleFilter(
              { value: removedValue, type: inputType },
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
