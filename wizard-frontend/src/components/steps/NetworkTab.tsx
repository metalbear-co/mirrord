import { useState, useContext } from "react";
import { Plus, Trash2, ArrowRight, Network } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

import { TabsContent } from "@/components/ui/tabs";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Separator } from "@/components/ui/separator";
import {
  addRemoveOrUpdateMapping,
  disableConfigFilter,
  disablePortsAndMapping,
  getLocalPort,
  readBoilerplateType,
  readCurrentFilters,
  readCurrentPortMapping,
  readCurrentPorts,
  readIncoming,
  removePortandMapping,
  UiHttpFilter,
  updateConfigFilter,
  updateConfigPortMapping,
  updateConfigPorts,
  updateIncoming,
} from "../JsonUtils";
import { ConfigDataContext } from "../UserDataContext";
import { Disable } from "react-disable";
import { PortMapping } from "./PortMapping";
import HttpFilter from "./HttpFilter";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../ui/select";
import AddNewFilter from "./AddNewFilter";

const IncomingConfigToggle = ({
  savedIncoming,
  setSavedIncoming,
}: {
  savedIncoming: any;
  setSavedIncoming: (any) => void;
}) => {
  const { config, setConfig } = useContext(ConfigDataContext);
  const [toggleEnabled, setToggleEnabled] = useState<boolean>(
    readBoilerplateType(config) === "replace" || readIncoming(config) !== false
  );

  return (
    <Button
      key={"incomingEnabledToggle"}
      variant={toggleEnabled ? "default" : "outline"}
      size="sm"
      className={`rounded-full px-4 py-2 font-mono transition-all ${
        toggleEnabled
          ? "bg-primary text-primary-foreground hover:bg-primary/90"
          : "hover:bg-primary/10"
      }`}
      onClick={() => {
        if (toggleEnabled) {
          // user turned incoming from "on" to "off"
          // save state, in case the user turns the toggle back on
          setSavedIncoming(readIncoming(config));
          const newConfig = updateIncoming(config, false);
          setConfig(newConfig);
        } else {
          // user turned incoming from "off" to "on"
          // restore the last state that was saved
          const newConfig = updateIncoming(config, savedIncoming);
          setConfig(newConfig);
        }

        setToggleEnabled(!toggleEnabled);
      }}
      disabled={readBoilerplateType(config) === "replace"}
    >
      {toggleEnabled ? "On" : "Off"}
    </Button>
  );
};

const FilterConfigToggle = ({
  toggleEnabled,
  setToggleEnabled,
}: {
  toggleEnabled: boolean;
  setToggleEnabled: (boolean) => void;
}) => {
  const { config, setConfig } = useContext(ConfigDataContext);
  const [savedFilters, setSavedFilters] = useState<{
    filters: UiHttpFilter[];
    operator: "any" | "all" | null;
  }>(readCurrentFilters(config));

  return (
    <Button
      key={"filtersEnabledToggle"}
      variant={toggleEnabled ? "default" : "outline"}
      size="sm"
      className={`rounded-full px-4 py-2 font-mono transition-all ${
        toggleEnabled
          ? "bg-primary text-primary-foreground hover:bg-primary/90"
          : "hover:bg-primary/10"
      }`}
      onClick={() => {
        if (toggleEnabled) {
          // user turned incoming from "on" to "off"
          // save state, in case the user turns the toggle back on
          setSavedFilters(readCurrentFilters(config));
          const newConfig = disableConfigFilter(config);
          setConfig(newConfig);
        } else {
          // user turned incoming from "off" to "on"
          // restore the last state that was saved
          const newConfig = updateConfigFilter(
            savedFilters.filters,
            savedFilters.operator,
            config
          );
          setConfig(newConfig);
        }

        setToggleEnabled(!toggleEnabled);
      }}
    >
      {toggleEnabled ? "On" : "Off"}
    </Button>
  );
};

const PortsConfigToggle = ({
  toggleEnabled,
  setToggleEnabled,
  detectedPorts,
}: {
  toggleEnabled: boolean;
  setToggleEnabled: (boolean) => void;
  detectedPorts: number[];
}) => {
  const { config, setConfig } = useContext(ConfigDataContext);
  const [savedPorts, setSavedPorts] = useState<any>([
    detectedPorts,
    readCurrentPortMapping(config),
  ]);

  return (
    <Button
      key={"filtersEnabledToggle"}
      variant={toggleEnabled ? "default" : "outline"}
      size="sm"
      className={`rounded-full px-4 py-2 font-mono transition-all ${
        toggleEnabled
          ? "bg-primary text-primary-foreground hover:bg-primary/90"
          : "hover:bg-primary/10"
      }`}
      onClick={() => {
        if (toggleEnabled) {
          // user turned incoming from "on" to "off"
          // save state, in case the user turns the toggle back on
          setSavedPorts([
            readCurrentPorts(config),
            readCurrentPortMapping(config),
          ]);
          const newConfig = disablePortsAndMapping(config);
          setConfig(newConfig);
        } else {
          // user turned incoming from "off" to "on"
          // restore the last state that was saved
          const [savedPortsOnly, savedMapping] = savedPorts;
          const partialNewConfig = updateConfigPorts(savedPortsOnly, config);
          const newConfig = updateConfigPortMapping(
            savedMapping,
            partialNewConfig
          );
          setConfig(newConfig);
        }

        setToggleEnabled(!toggleEnabled);
      }}
    >
      {toggleEnabled ? "On" : "Off"}
    </Button>
  );
};

const NetworkTab = ({
  savedIncoming,
  setSavedIncoming,
}: {
  savedIncoming: any;
  setSavedIncoming: (any) => void;
}) => {
  const { config, setConfig } = useContext(ConfigDataContext);
  const [toggleFiltersEnabled, setToggleFiltersEnabled] =
    useState<boolean>(false);
  const [togglePortsEnabled, setTogglePortsEnabled] = useState<boolean>(false);
  const [newRemotePort, setNewRemotePort] = useState<number>();

  const mockPorts = [8080, 3000, 5432, 9000, 4000, 6379, 5672, 3306];

  const handleOnSubmit = (e) => {
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

  return (
    <TabsContent value="network" className="space-y-3 mt-4">
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="flex items-center gap-2 text-base">
            <Network className="h-4 w-4" />
            Incoming Traffic
            <IncomingConfigToggle
              savedIncoming={savedIncoming}
              setSavedIncoming={setSavedIncoming}
            />
          </CardTitle>
        </CardHeader>
        {/* Controlled by IncomingConfigToggle indirectly (through incoming config state) */}
        <Disable disabled={readIncoming(config) === false}>
          <CardContent>
            <div className="space-y-4">
              <div className="space-y-4">
                {readBoilerplateType(config) !== "replace" && (
                  <div className="gap-2">
                    <h3 className="text-base font-semibold mb-1 flex items-center gap-2 justify-center">
                      Traffic Filtering
                      <FilterConfigToggle
                        toggleEnabled={toggleFiltersEnabled}
                        setToggleEnabled={setToggleFiltersEnabled}
                      />
                    </h3>
                    <p className="text-sm text-muted-foreground mb-4">
                      mirrord supports stealing a subset of the remote target's
                      traffic. You can do this by specifying a filter on either
                      an HTTP header or path.
                    </p>
                  </div>
                )}

                {/* HTTP Filters */}
                {readBoilerplateType(config) !== "replace" &&
                  toggleFiltersEnabled && (
                    <div className="space-y-4">
                      <div className="space-y-3">
                        {/* Header Filtering */}
                        <div className="space-y-2">
                          <div className="flex-1 items-center justify-between">
                            <Label className="font-medium">
                              Header Filters
                            </Label>
                            <AddNewFilter
                              type="header"
                              placeholder="eg. x-mirrord-test: true"
                              key="addheaderfilter"
                            />
                          </div>
                          {readCurrentFilters(config).filters.length > 0 && (
                            <div className="space-y-3">
                              {readCurrentFilters(config)
                                .filters.filter(
                                  (filter) => filter.type === "header"
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

                        {/* Path Filtering */}
                        <div className="space-y-2">
                          <div className="flex-1 items-center justify-between">
                            <Label className="font-medium">Path Filters</Label>
                            <AddNewFilter
                              type="path"
                              placeholder="e.g. /api/v1/test"
                              key="addpathfilter"
                            />
                          </div>

                          {readCurrentFilters(config).filters.length > 0 && (
                            <div className="space-y-3">
                              {readCurrentFilters(config)
                                .filters.filter(
                                  (filter) => filter.type === "path"
                                )
                                .map((headerFilter) => (
                                  <HttpFilter
                                    initValue={headerFilter.value}
                                    inputType={"path"}
                                    key={headerFilter.value}
                                  />
                                ))}
                            </div>
                          )}
                        </div>

                        {/* Filter Logic Selection - Only show when there are multiple filters */}
                        {readCurrentFilters(config).filters.length > 1 && (
                          <>
                            <Separator />
                            <div className="space-y-2">
                              <Label className="font-medium">
                                Filter Logic
                              </Label>
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
                                    config
                                  );
                                  setConfig(newConfig);
                                }}
                              >
                                <div className="flex items-center space-x-2">
                                  <RadioGroupItem value="all" id="and" />
                                  <Label htmlFor="and" className="text-sm">
                                    <strong>All</strong> - Match all specified
                                    filters
                                  </Label>
                                </div>
                                <div className="flex items-center space-x-2">
                                  <RadioGroupItem value="any" id="or" />
                                  <Label htmlFor="or" className="text-sm">
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

                {/* Simplified Port Configuration */}
                <div className="space-y-4 mt-6">
                  <div>
                    <h3 className="text-base font-semibold mb-1 flex items-center gap-2 justify-center">
                      Port Configuration
                      <PortsConfigToggle
                        toggleEnabled={togglePortsEnabled}
                        setToggleEnabled={setTogglePortsEnabled}
                        detectedPorts={mockPorts}
                      />
                    </h3>
                    <p className="text-xs text-muted-foreground">
                      {readBoilerplateType(config) === "replace"
                        ? "Add port mappings for ports that differ locally and remotely. Traffic on all ports will be stolen."
                        : "Add, remove or map ports for traffic mirroring/ stealing."}
                    </p>
                  </div>

                  {togglePortsEnabled && (
                    <div className="space-y-4">
                      <p className="text-xs text-muted-foreground mb-3">
                        {mockPorts.length} ports were detected automatically in
                        the target.
                      </p>
                      <div className="space-y-3">
                        <div className="space-y-2">
                          {/* Remote Port and Local Port column labels */}
                          <div key="labels">
                            <div className="flex gap-3">
                              <div className="flex-1">
                                <Label className="text-xs text-muted-foreground">
                                  Remote Port
                                </Label>
                              </div>

                              <div className="flex-1">
                                <Label className="text-xs text-muted-foreground">
                                  Local Port
                                </Label>
                              </div>
                            </div>
                          </div>

                          {readCurrentPorts(config).map((remotePort) => (
                            <PortMapping
                              key={remotePort}
                              remotePort={remotePort}
                              detectedPort={mockPorts.includes(remotePort)}
                            />
                          ))}

                          {/* add new remote port without (initial) mapping */}
                          <div
                            key="addports"
                            className="border rounded-lg p-3 space-y-3 bg-green-100"
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
                                  value={newRemotePort}
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

                              {/* Add button */}
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
                    </div>
                  )}
                </div>
              </div>
            </div>
          </CardContent>
        </Disable>
      </Card>
    </TabsContent>
  );
};

export default NetworkTab;
