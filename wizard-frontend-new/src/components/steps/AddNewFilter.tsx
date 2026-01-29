import { Plus } from "lucide-react";
import {
  Button,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@metalbear/ui";
import {
  readCurrentFilters,
  regexificationRay,
  updateConfigFilter,
} from "../JsonUtils";
import { useContext, useState, type FormEvent } from "react";
import { ConfigDataContext } from "../UserDataContext";

export const AddNewFilter = ({
  type,
  placeholder,
}: {
  type: "header" | "path";
  placeholder: string;
}) => {
  const { config, setConfig } = useContext(ConfigDataContext)!;
  const [inputMatching, setInputMatching] = useState<"exact" | "regex">(
    "regex",
  );
  const [inputValue, setInputValue] = useState<string>();
  const handleOnSubmit = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    if (inputValue) {
      const { filters, operator } = readCurrentFilters(config);
      const newValue =
        inputMatching === "exact" ? regexificationRay(inputValue) : inputValue;

      if (filters.filter((filter) => filter.value == newValue).length === 0) {
        const updated = updateConfigFilter(
          filters.concat([
            {
              value: newValue,
              type: type,
            },
          ]),
          operator,
          config,
        );
        setConfig(updated);
      }

      setInputValue("");
    }
  };

  return (
    <div key="addfilter" className="border border-[var(--border)] rounded-lg p-3 space-y-3">
      <form onSubmit={handleOnSubmit} className="flex items-center gap-3">
        <Select
          value={inputMatching}
          onValueChange={(value: "exact" | "regex") => {
            setInputMatching(value);
          }}
        >
          <SelectTrigger className="w-40">
            <SelectValue />
          </SelectTrigger>
          <SelectContent className="bg-[var(--card)] border border-[var(--border)]">
            <SelectItem value="exact">Exact Match</SelectItem>
            <SelectItem value="regex">Regex Match</SelectItem>
          </SelectContent>
        </Select>

        <div className="flex-1">
          <Input
            type="text"
            className="font-mono"
            value={inputValue}
            placeholder={placeholder}
            onChange={(event) => {
              if (event.target.value) {
                setInputValue(event.target.value);
              } else {
                setInputValue("");
              }
            }}
          />
        </div>

        <Button type="submit" variant="outline" size="sm">
          <Plus className="h-4 w-4" /> Add
        </Button>
      </form>
    </div>
  );
};

export default AddNewFilter;
