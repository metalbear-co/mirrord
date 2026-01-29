import { useContext, useEffect, useState } from "react";
import { Network, Plus, Trash2 } from "lucide-react";
import {
  Button,
  Input,
  Label,
  Switch,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  RadioGroup,
  RadioGroupItem,
} from "@metalbear/ui";
import { ConfigDataContext } from "../UserDataContext";
import {
  readBoilerplateType,
  readCurrentFilters,
  readCurrentPorts,
  readCurrentPortMapping,
  updateConfigFilter,
  disableConfigFilter,
  updateConfigPorts,
  addRemoveOrUpdateMapping,
  removePortandMapping,
  getLocalPort,
  regexificationRay,
  type UiHttpFilter,
} from "../JsonUtils";
import type { ToggleableConfigFor_IncomingFileConfig } from "../../mirrord-schema";

interface NetworkTabProps {
  savedIncoming: ToggleableConfigFor_IncomingFileConfig;
  targetPorts: number[];
  setSavedIncoming: (incoming: ToggleableConfigFor_IncomingFileConfig) => void;
  setPortConflicts: (hasConflicts: boolean) => void;
}

const NetworkTab = ({
  savedIncoming: _savedIncoming,
  targetPorts: _targetPorts,
  setSavedIncoming: _setSavedIncoming,
  setPortConflicts,
}: NetworkTabProps) => {
  // These props are kept for API compatibility but not currently used:
  // _savedIncoming, _targetPorts, _setSavedIncoming
  void _savedIncoming;
  void _targetPorts;
  void _setSavedIncoming;
  const { config, setConfig } = useContext(ConfigDataContext)!;
  const [filtersEnabled, setFiltersEnabled] = useState(false);
  const [newFilterType, setNewFilterType] = useState<"header" | "path">("header");
  const [newFilterValue, setNewFilterValue] = useState("");
  const [newPortValue, setNewPortValue] = useState("");

  const boilerplateType = readBoilerplateType(config);
  const isReplaceMode = boilerplateType === "replace";
  const isStealMode = boilerplateType === "steal";

  const { filters, operator } = readCurrentFilters(config);
  const currentPorts = readCurrentPorts(config);
  const portMappings = readCurrentPortMapping(config);

  // Check for port conflicts
  useEffect(() => {
    const localPorts = portMappings.map(([local]) => local);
    const portsWithoutMapping = currentPorts.filter(
      (port) => !portMappings.some(([, remote]) => remote === port)
    );
    const allLocalPorts = [...localPorts, ...portsWithoutMapping];
    const hasConflicts = new Set(allLocalPorts).size !== allLocalPorts.length;
    setPortConflicts(hasConflicts);
  }, [currentPorts, portMappings, setPortConflicts]);

  // Sync filters enabled state
  useEffect(() => {
    setFiltersEnabled(filters.length > 0);
  }, [filters.length]);

  const handleFiltersToggle = (enabled: boolean) => {
    if (enabled) {
      setFiltersEnabled(true);
    } else {
      setFiltersEnabled(false);
      setConfig(disableConfigFilter(config));
    }
  };

  const handleAddFilter = () => {
    if (!newFilterValue.trim()) return;

    const newFilter: UiHttpFilter = {
      type: newFilterType,
      value: regexificationRay(newFilterValue.trim()),
    };

    const updatedConfig = updateConfigFilter(
      [...filters, newFilter],
      operator || "any",
      config
    );
    setConfig(updatedConfig);
    setNewFilterValue("");
  };

  const handleRemoveFilter = (filterToRemove: UiHttpFilter) => {
    const newFilters = filters.filter(
      (f) => f.type !== filterToRemove.type || f.value !== filterToRemove.value
    );
    if (newFilters.length === 0) {
      setConfig(disableConfigFilter(config));
      setFiltersEnabled(false);
    } else {
      setConfig(updateConfigFilter(newFilters, operator, config));
    }
  };

  const handleOperatorChange = (newOperator: "any" | "all") => {
    setConfig(updateConfigFilter(filters, newOperator, config));
  };

  const handleAddPort = () => {
    const port = parseInt(newPortValue, 10);
    if (isNaN(port) || port <= 0 || port > 65535) return;
    if (currentPorts.includes(port)) return;

    setConfig(updateConfigPorts([...currentPorts, port], config));
    setNewPortValue("");
  };

  const handleRemovePort = (port: number) => {
    setConfig(removePortandMapping(port, config));
  };

  const handlePortMappingChange = (remotePort: number, localPort: string) => {
    const local = parseInt(localPort, 10);
    if (isNaN(local) || local <= 0 || local > 65535) return;
    setConfig(addRemoveOrUpdateMapping(remotePort, local, config));
  };

  return (
    <div className="space-y-6">
      {/* Ports Section */}
      <div>
        <div className="flex items-center gap-3 pb-4 border-b border-[var(--border)]">
          <div className="w-10 h-10 rounded-lg bg-primary/10 flex items-center justify-center">
            <Network className="h-5 w-5 text-primary" />
          </div>
          <div>
            <h3 className="text-lg font-semibold">Ports</h3>
            <p className="text-sm text-[var(--muted-foreground)]">
              Configure which ports to intercept from the remote target
            </p>
          </div>
        </div>
        <div className="space-y-4 pt-4">
          {/* Add new port */}
          {!isReplaceMode && (
            <div className="flex gap-2">
              <Input
                type="number"
                placeholder="Add port (e.g., 8080)"
                value={newPortValue}
                onChange={(e) => setNewPortValue(e.target.value)}
                className="flex-grow"
                min={1}
                max={65535}
              />
              <Button onClick={handleAddPort} variant="outline">
                <Plus className="h-4 w-4 mr-2" />
                Add
              </Button>
            </div>
          )}

          {/* Port list */}
          {currentPorts.length > 0 ? (
            <div className="space-y-2">
              {currentPorts.map((port) => {
                const localPort = getLocalPort(port, config);
                const isMapped = localPort !== port;

                return (
                  <div
                    key={port}
                    className="flex items-center justify-between p-3 rounded-lg border border-[var(--border)] bg-[var(--card)]"
                  >
                    <div className="flex items-center gap-4">
                      <span className="font-medium text-[var(--foreground)]">
                        Remote: {port}
                      </span>
                      <span className="text-[var(--muted-foreground)]">&rarr;</span>
                      <div className="flex items-center gap-2">
                        <span className="text-sm text-[var(--muted-foreground)]">
                          Local:
                        </span>
                        <Input
                          type="number"
                          value={localPort}
                          onChange={(e) =>
                            handlePortMappingChange(port, e.target.value)
                          }
                          className="w-24 h-8"
                          min={1}
                          max={65535}
                        />
                        {isMapped && (
                          <span className="text-xs text-primary">(mapped)</span>
                        )}
                      </div>
                    </div>
                    {!isReplaceMode && (
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => handleRemovePort(port)}
                        className="text-destructive hover:text-destructive"
                      >
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    )}
                  </div>
                );
              })}
            </div>
          ) : (
            <div className="p-5 text-center text-sm text-[var(--muted-foreground)] bg-gradient-to-br from-primary/5 to-transparent rounded-lg border border-dashed border-[var(--border)]">
              <Network className="h-6 w-6 mx-auto mb-2 opacity-40" />
              {isReplaceMode
                ? "All traffic will be intercepted in replace mode"
                : "No ports configured. Add ports above or they will be auto-detected."}
            </div>
          )}
        </div>
      </div>

      {/* Filters Section - Only for Steal mode */}
      {isStealMode && (
        <div className="pt-6 border-t border-[var(--border)]">
          <div className="flex items-center justify-between pb-4">
            <div className="flex items-center gap-3">
              <div className="w-10 h-10 rounded-lg bg-secondary/10 flex items-center justify-center">
                <Network className="h-5 w-5 text-secondary" />
              </div>
              <div>
                <h3 className="text-lg font-semibold">HTTP Filters</h3>
                <p className="text-sm text-[var(--muted-foreground)]">
                  Selectively steal traffic based on headers or paths
                </p>
              </div>
            </div>
            <div className="flex items-center gap-3 px-3 py-2 rounded-lg border border-[var(--border)] bg-[var(--muted)]/30">
              <span className="text-sm font-medium text-[var(--foreground)]">
                {filtersEnabled ? "Enabled" : "Disabled"}
              </span>
              <Switch
                checked={filtersEnabled}
                onCheckedChange={handleFiltersToggle}
                className="data-[state=checked]:bg-primary border border-[var(--border)]"
              />
            </div>
          </div>
          {filtersEnabled && (
            <div className="space-y-4">
              {/* Operator selection */}
              {filters.length > 1 && (
                <div className="space-y-2">
                  <Label>Match</Label>
                  <RadioGroup
                    value={operator || "any"}
                    onValueChange={(value: string) =>
                      handleOperatorChange(value as "any" | "all")
                    }
                    className="flex gap-4"
                  >
                    <div className="flex items-center space-x-2">
                      <RadioGroupItem value="any" id="any" />
                      <Label htmlFor="any" className="font-normal cursor-pointer">
                        Any filter (OR)
                      </Label>
                    </div>
                    <div className="flex items-center space-x-2">
                      <RadioGroupItem value="all" id="all" />
                      <Label htmlFor="all" className="font-normal cursor-pointer">
                        All filters (AND)
                      </Label>
                    </div>
                  </RadioGroup>
                </div>
              )}

              {/* Add new filter */}
              <div className="flex gap-2">
                <Select
                  value={newFilterType}
                  onValueChange={(value: string) =>
                    setNewFilterType(value as "header" | "path")
                  }
                >
                  <SelectTrigger className="w-32">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent className="bg-[var(--card)] border border-[var(--border)]">
                    <SelectItem value="header">Header</SelectItem>
                    <SelectItem value="path">Path</SelectItem>
                  </SelectContent>
                </Select>
                <Input
                  placeholder={
                    newFilterType === "header"
                      ? "x-custom-header: value"
                      : "/api/users"
                  }
                  value={newFilterValue}
                  onChange={(e) => setNewFilterValue(e.target.value)}
                  className="flex-grow"
                />
                <Button onClick={handleAddFilter} variant="outline">
                  <Plus className="h-4 w-4 mr-2" />
                  Add
                </Button>
              </div>

              {/* Filter list */}
              {filters.length > 0 ? (
                <div className="space-y-2">
                  {filters.map((filter, index) => (
                    <div
                      key={`${filter.type}-${filter.value}-${index}`}
                      className="flex items-center justify-between p-3 rounded-lg border border-[var(--border)] bg-[var(--card)]"
                    >
                      <div className="flex items-center gap-3">
                        <span className="text-xs px-2 py-1 rounded bg-[var(--muted)] text-[var(--muted-foreground)] uppercase font-medium">
                          {filter.type}
                        </span>
                        <code className="text-sm font-code text-[var(--foreground)]">
                          {filter.value}
                        </code>
                      </div>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => handleRemoveFilter(filter)}
                        className="text-destructive hover:text-destructive"
                      >
                        <Trash2 className="h-4 w-4" />
                      </Button>
                    </div>
                  ))}
                </div>
              ) : (
                <div className="p-5 text-center text-sm text-[var(--muted-foreground)] bg-gradient-to-br from-secondary/5 to-transparent rounded-lg border border-dashed border-[var(--border)]">
                  No filters configured. All matching traffic will be stolen.
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
};

export default NetworkTab;
