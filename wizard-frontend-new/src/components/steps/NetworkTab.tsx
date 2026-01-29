import { useState, useContext, type FormEvent } from "react";
import { Plus, Network } from "lucide-react";
import {
  Button,
  Input,
  Label,
  RadioGroup,
  RadioGroupItem,
  Separator,
  Switch,
} from "@metalbear/ui";
import {
  disableConfigFilter,
  disablePortsAndMapping,
  readBoilerplateType,
  readCurrentFilters,
  readCurrentPortMapping,
  readCurrentPorts,
  readIncoming,
  type UiHttpFilter,
  updateConfigFilter,
  updateConfigPortMapping,
  updateConfigPorts,
  updateIncoming,
} from "../JsonUtils";
import { ConfigDataContext } from "../UserDataContext";
import type {
  ToggleableConfigFor_IncomingFileConfig,
  PortMapping,
} from "../../mirrord-schema";
import HttpFilter from "./HttpFilter";
import AddNewFilter from "./AddNewFilter";
import PortMappingEntry from "./PortMapping";

const IncomingConfigToggle = ({
  savedIncoming,
  setSavedIncoming,
}: {
  savedIncoming: ToggleableConfigFor_IncomingFileConfig;
  setSavedIncoming: (value: ToggleableConfigFor_IncomingFileConfig) => void;
}) => {
  const { config, setConfig } = useContext(ConfigDataContext)!;
  const [toggleEnabled, setToggleEnabled] = useState<boolean>(
    readBoilerplateType(config) === "replace" || readIncoming(config) !== false,
  );

  return (
    <Switch
      key={"incomingEnabledToggle"}
      checked={toggleEnabled}
      onClick={() => {
        if (toggleEnabled) {
          setSavedIncoming(readIncoming(config));
          const newConfig = updateIncoming(config, false);
          setConfig(newConfig);
        } else {
          const newConfig = updateIncoming(config, savedIncoming);
          setConfig(newConfig);
        }

        setToggleEnabled(!toggleEnabled);
      }}
      disabled={readBoilerplateType(config) === "replace"}
    />
  );
};

const FilterConfigToggle = ({
  toggleEnabled,
  setToggleEnabled,
}: {
  toggleEnabled: boolean;
  setToggleEnabled: (enabled: boolean) => void;
}) => {
  const { config, setConfig } = useContext(ConfigDataContext)!;
  const [savedFilters, setSavedFilters] = useState<{
    filters: UiHttpFilter[];
    operator: "any" | "all" | null;
  }>(readCurrentFilters(config));

  return (
    <Switch
      key={"filtersEnabledToggle"}
      checked={toggleEnabled}
      onClick={() => {
        if (toggleEnabled) {
          setSavedFilters(readCurrentFilters(config));
          const newConfig = disableConfigFilter(config);
          setConfig(newConfig);
        } else {
          const newConfig = updateConfigFilter(
            savedFilters.filters,
            savedFilters.operator,
            config,
          );
          setConfig(newConfig);
        }

        setToggleEnabled(!toggleEnabled);
      }}
    />
  );
};

const PortsConfigToggle = ({
  toggleEnabled,
  setToggleEnabled,
  detectedPorts,
}: {
  toggleEnabled: boolean;
  setToggleEnabled: (enabled: boolean) => void;
  detectedPorts: number[];
}) => {
  const { config, setConfig } = useContext(ConfigDataContext)!;
  const [savedPorts, setSavedPorts] = useState<[number[], PortMapping]>([
    detectedPorts,
    readCurrentPortMapping(config),
  ]);

  return (
    <Switch
      key={"portsEnabledToggle"}
      checked={toggleEnabled}
      onClick={() => {
        if (toggleEnabled) {
          setSavedPorts([
            readCurrentPorts(config),
            readCurrentPortMapping(config),
          ]);
          const newConfig = disablePortsAndMapping(config);
          setConfig(newConfig);
        } else {
          const [savedPortsOnly, savedMapping] = savedPorts;
          const partialNewConfig = updateConfigPorts(savedPortsOnly, config);
          const newConfig = updateConfigPortMapping(
            savedMapping,
            partialNewConfig,
          );
          setConfig(newConfig);
        }

        setToggleEnabled(!toggleEnabled);
      }}
    />
  );
};

const NetworkTab = ({
  savedIncoming,
  targetPorts,
  setSavedIncoming,
  setPortConflicts,
}: {
  savedIncoming: ToggleableConfigFor_IncomingFileConfig;
  targetPorts: number[];
  setSavedIncoming: (value: ToggleableConfigFor_IncomingFileConfig) => void;
  setPortConflicts: (value: boolean) => void;
}) => {
  const { config, setConfig } = useContext(ConfigDataContext)!;
  const [toggleFiltersEnabled, setToggleFiltersEnabled] =
    useState<boolean>(false);
  const [togglePortsEnabled, setTogglePortsEnabled] = useState<boolean>(false);
  const [newRemotePort, setNewRemotePort] = useState<number>();

  const handleOnSubmit = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    if (newRemotePort) {
      const ports = readCurrentPorts(config);
      const newConfig = ports.includes(newRemotePort)
        ? config
        : updateConfigPorts(ports.concat([newRemotePort]), config);
      setConfig(newConfig);

      setNewRemotePort(undefined);
    }
  };

  const incomingDisabled = readIncoming(config) === false;

  return (
    <div className="space-y-6">
      {/* Incoming Traffic Section */}
      <div>
        <div className="flex items-center gap-3 pb-4 border-b border-[var(--border)]">
          <div className="w-10 h-10 rounded-lg bg-primary/10 flex items-center justify-center">
            <Network className="h-5 w-5 text-primary" />
          </div>
          <div className="flex-grow">
            <h3 className="text-lg font-semibold">Incoming Traffic</h3>
            <p className="text-sm text-[var(--muted-foreground)]">
              Configure how incoming traffic is handled
            </p>
          </div>
          <IncomingConfigToggle
            savedIncoming={savedIncoming}
            setSavedIncoming={setSavedIncoming}
          />
        </div>

        <div className={incomingDisabled ? "opacity-50 pointer-events-none" : ""}>
          <div className="space-y-6 pt-4">
            {/* Traffic Filtering */}
            {readBoilerplateType(config) !== "replace" && (
              <div className="space-y-4">
                <div className="flex items-center justify-between">
                  <div>
                    <h4 className="text-base font-semibold">Traffic Filtering</h4>
                    <p className="text-sm text-[var(--muted-foreground)]">
                      Steal a subset of traffic by specifying HTTP header or path filters
                    </p>
                  </div>
                  <div className="flex items-center gap-3 px-3 py-2 rounded-lg border border-[var(--border)] bg-[var(--muted)]/30">
                    <span className="text-sm font-medium text-[var(--foreground)]">
                      {toggleFiltersEnabled ? "Enabled" : "Disabled"}
                    </span>
                    <FilterConfigToggle
                      toggleEnabled={toggleFiltersEnabled}
                      setToggleEnabled={setToggleFiltersEnabled}
                    />
                  </div>
                </div>

                {toggleFiltersEnabled && (
                  <div className="space-y-4 pl-4 border-l-2 border-primary/20">
                    <div className="space-y-3">
                      <div className="space-y-2">
                        <Label className="font-medium">Header Filters</Label>
                        <AddNewFilter
                          type="header"
                          placeholder="eg. x-mirrord-test: true"
                          key="addheaderfilter"
                        />
                        {readCurrentFilters(config).filters.length > 0 && (
                          <div className="space-y-3">
                            {readCurrentFilters(config)
                              .filters.filter(
                                (filter) => filter.type === "header",
                              )
                              .map((headerFilter) => (
                                <HttpFilter
                                  initValue={headerFilter.value}
                                  inputType={"header"}
                                  key={headerFilter.value}
                                />
                              ))}
                          </div>
                        )}
                      </div>

                      <div className="space-y-2">
                        <Label className="font-medium">Path Filters</Label>
                        <AddNewFilter
                          type="path"
                          placeholder="eg. /api/v1/test"
                          key="addpathfilter"
                        />
                        {readCurrentFilters(config).filters.length > 0 && (
                          <div className="space-y-3">
                            {readCurrentFilters(config)
                              .filters.filter(
                                (filter) => filter.type === "path",
                              )
                              .map((pathFilter) => (
                                <HttpFilter
                                  initValue={pathFilter.value}
                                  inputType={"path"}
                                  key={pathFilter.value}
                                />
                              ))}
                          </div>
                        )}
                      </div>

                      {readCurrentFilters(config).filters.length > 1 && (
                        <>
                          <Separator />
                          <div className="space-y-2">
                            <Label className="font-medium">Filter Logic</Label>
                            <RadioGroup
                              value={
                                readCurrentFilters(config).operator ?? "all"
                              }
                              onValueChange={(value: "all" | "any") => {
                                const existingFilters =
                                  readCurrentFilters(config).filters;
                                const newConfig = updateConfigFilter(
                                  existingFilters,
                                  value,
                                  config,
                                );
                                setConfig(newConfig);
                              }}
                            >
                              <div className="flex items-center space-x-2">
                                <RadioGroupItem value="all" id="and" />
                                <Label htmlFor="and" className="text-sm font-normal cursor-pointer">
                                  <strong>All</strong> - Match all specified
                                  filters
                                </Label>
                              </div>
                              <div className="flex items-center space-x-2">
                                <RadioGroupItem value="any" id="or" />
                                <Label htmlFor="or" className="text-sm font-normal cursor-pointer">
                                  <strong>Any</strong> - Match any specified
                                  filter
                                </Label>
                              </div>
                            </RadioGroup>
                          </div>
                        </>
                      )}
                    </div>
                  </div>
                )}
              </div>
            )}

            {/* Port Configuration */}
            <div className="space-y-4">
              <div className="flex items-center justify-between">
                <div>
                  <h4 className="text-base font-semibold">Port Configuration</h4>
                  <p className="text-sm text-[var(--muted-foreground)]">
                    {readBoilerplateType(config) === "replace"
                      ? "Add port mappings for ports that differ locally and remotely"
                      : "Add, remove or map ports for traffic mirroring/stealing"}
                  </p>
                </div>
                <div className="flex items-center gap-3 px-3 py-2 rounded-lg border border-[var(--border)] bg-[var(--muted)]/30">
                  <span className="text-sm font-medium text-[var(--foreground)]">
                    {togglePortsEnabled ? "Enabled" : "Disabled"}
                  </span>
                  <PortsConfigToggle
                    toggleEnabled={togglePortsEnabled}
                    setToggleEnabled={setTogglePortsEnabled}
                    detectedPorts={targetPorts}
                  />
                </div>
              </div>

              {togglePortsEnabled && (
                <div className="space-y-4 pl-4 border-l-2 border-primary/20">
                  <p className="text-xs text-[var(--muted-foreground)]">
                    {targetPorts.length} ports were detected automatically in the target.
                  </p>
                  <div className="space-y-3">
                    {readCurrentPorts(config).length > 0 && (
                      <div key="labels">
                        <div className="flex gap-3">
                          <div className="flex-1">
                            <Label className="text-xs text-[var(--muted-foreground)]">
                              Remote Port
                            </Label>
                          </div>
                          <div className="flex-1">
                            <Label className="text-xs text-[var(--muted-foreground)]">
                              Local Port
                            </Label>
                          </div>
                          <div className="w-8" /> {/* Spacer for delete button */}
                        </div>
                      </div>
                    )}

                    {readCurrentPorts(config).map((remotePort) => (
                      <PortMappingEntry
                        key={remotePort}
                        remotePort={remotePort}
                        detectedPort={targetPorts.includes(remotePort)}
                        setPortConflicts={setPortConflicts}
                      />
                    ))}

                    <div
                      key="addports"
                      className="border border-[var(--border)] rounded-lg p-3 space-y-3 bg-green-100 dark:bg-green-900/20"
                    >
                      <form
                        onSubmit={handleOnSubmit}
                        className="flex items-center gap-3"
                      >
                        <Label>Add New Port</Label>
                        <div className="flex-1">
                          <Input
                            type="text"
                            pattern="[0-9]*"
                            className="font-mono"
                            value={newRemotePort ?? ""}
                            placeholder="Remote Port Number"
                            onChange={(event) => {
                              if (event.target.value) {
                                setNewRemotePort(+event.target.value);
                              } else {
                                setNewRemotePort(undefined);
                              }
                            }}
                          />
                        </div>

                        <Button
                          type="submit"
                          variant="ghost"
                          size="sm"
                          className="h-8 w-8 p-0 text-green-500 hover:text-green-600 hover:bg-green-50"
                        >
                          <Plus className="h-4 w-4" />
                        </Button>
                      </form>
                    </div>
                  </div>
                </div>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
};

export default NetworkTab;
