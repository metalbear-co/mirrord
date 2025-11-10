import { useToast } from "@/hooks/use-toast";
import { useState, useContext } from "react";
import {
  Copy,
  Save,
  Server,
  Check,
  AlertCircle,
  ChevronDown,
  Plus,
  Trash2,
  ArrowRight,
  Network,
  ChevronLeft,
  ChevronRight,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  CardDescription,
} from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Separator } from "@/components/ui/separator";
import { Checkbox } from "@/components/ui/checkbox";
import { Textarea } from "@/components/ui/textarea";
import DownloadButton from "../DownloadButton";
import {
  disableConfigFilter,
  disablePortsAndMapping,
  getConfigString,
  readCurrentFilters,
  readCurrentPortMapping,
  readCurrentPorts,
  readCurrentTargetDetails,
  readIncoming,
  updateConfigFilter,
  updateConfigPortMapping,
  updateConfigPorts,
  updateConfigTarget,
  updateIncoming,
  validateJson,
} from "../JsonUtils";
import { ConfigDataContext, DefaultConfig } from "../UserDataContext";
import { LayerFileConfig, PortMapping } from "@/mirrord-schema";
import { Disable } from "react-disable";

const IncomingConfigToggle = ({
  savedIncoming,
  setSavedIncoming,
}: {
  savedIncoming: any;
  setSavedIncoming: (any) => void;
}) => {
  const { config, setConfig } = useContext(ConfigDataContext);
  const [toggleEnabled, setToggleEnabled] = useState<boolean>(
    readIncoming(config) !== false
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
  const [savedFilters, setSavedFilters] = useState<any>(
    readCurrentFilters(config)
  );

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
          const newConfig = updateConfigFilter(savedFilters, config);
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
}: {
  toggleEnabled: boolean;
  setToggleEnabled: (boolean) => void;
}) => {
  const { config, setConfig } = useContext(ConfigDataContext);
  const [savedPorts, setSavedPorts] = useState<any>([readCurrentPorts(config), readCurrentPortMapping(config)]);

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
          setSavedPorts([readCurrentPorts(config), readCurrentPortMapping(config)]); 
          const newConfig = disablePortsAndMapping(config); 
          setConfig(newConfig);
        } else {
          // user turned incoming from "off" to "on"
          // restore the last state that was saved
          const [savedPortsOnly, savedMapping] = savedPorts;
          const partialNewConfig = updateConfigPorts(savedPortsOnly, config);
          const newConfig = updateConfigPortMapping(savedMapping, partialNewConfig);
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

  const mockPorts = [8080, 3000, 5432, 9000, 4000, 6379, 5672, 3306];

  return (
    <TabsContent value="network" className="space-y-3 mt-4">
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="flex items-center gap-2 text-base">
            <Network className="h-4 w-4" />
            Incoming Traffic
            <IncomingConfigToggle savedIncoming={savedIncoming} setSavedIncoming={setSavedIncoming} />
          </CardTitle>
        </CardHeader>
        {/* Controlled by IncomingConfigToggle indirectly (through incoming config state) */}
        <Disable disabled={readIncoming(config) === false}>
          <CardContent>
            <div className="space-y-4">
              <div className="space-y-4">
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
                    traffic. You can do this by specifying a filter on either an
                    HTTP header or path.
                  </p>
                </div>

                {/* HTTP Filters */}
                {toggleFiltersEnabled && (
                  <div className="space-y-4">
                    <div className="space-y-3">
                      {/* Header Filtering */}
                      <div className="space-y-2">
                        <div className="flex items-center justify-between">
                          <Label className="font-medium">Header Filters</Label>
                          <Button
                            type="button"
                            variant="outline"
                            size="sm"
                            onClick={() => {
                              const {
                                header: existingH,
                                path: existingP,
                                operator,
                              } = readCurrentFilters(config);
                              const updated = updateConfigFilter(
                                {
                                  headerFilters: existingH.concat(["header"]),
                                  pathFilters: existingP,
                                  operator,
                                },
                                config
                              );
                              setConfig(updated);
                            }}
                          >
                            <Plus className="h-4 w-4 mr-1" />
                            Add
                          </Button>
                        </div>
                        {readCurrentFilters(config).header.length > 0 && (
                          <div className="space-y-3">
                            {readCurrentFilters(config).header.map(
                              (headerFilterString, index) => (
                                <div key={index} className="space-y-2">
                                  <div className="flex items-center gap-2">
                                    <Input
                                      placeholder="e.g., x-mirrord-test: true"
                                      value={headerFilterString}
                                      onChange={(event) => {
                                        // todo: option 1 = update entire filter with updateConfigFilter()
                                        // todo: option 2 = write function to add/ update single filter
                                      }}
                                      className="flex-1"
                                    />
                                    <Select
                                      value={filter.matchType || "exact"}
                                      onValueChange={(
                                        value: "exact" | "regex"
                                      ) => {
                                        const newFilters = [
                                          ...config.feature.network.incoming
                                            .httpFilter,
                                        ];
                                        const headerIndex =
                                          newFilters.findIndex(
                                            (f, i) =>
                                              f.type === "header" && i === index
                                          );
                                        if (headerIndex !== -1) {
                                          newFilters[headerIndex] = {
                                            ...newFilters[headerIndex],
                                            matchType: value,
                                          };
                                          setConfig({
                                            ...config,
                                            network: {
                                              ...config.feature.network,
                                              incoming: {
                                                ...config.feature.network
                                                  .incoming,
                                                httpFilter: newFilters,
                                              },
                                            },
                                          });
                                        }
                                      }}
                                    >
                                      <SelectTrigger className="w-20">
                                        <SelectValue />
                                      </SelectTrigger>
                                      <SelectContent>
                                        <SelectItem value="exact">
                                          Exact
                                        </SelectItem>
                                        <SelectItem value="regex">
                                          Regex
                                        </SelectItem>
                                      </SelectContent>
                                    </Select>
                                    <Button
                                      type="button"
                                      variant="outline"
                                      size="sm"
                                      onClick={() => {
                                        // TODO
                                        // const newFilters = (
                                        //   config.feature.network.incoming
                                        //     .httpFilter ?? []
                                        // ).filter(
                                        //   (f, i) =>
                                        //     !(
                                        //       f.type === "header" &&
                                        //       i === index
                                        //     )
                                        // );
                                        // setConfig({
                                        //   ...config,
                                        //   network: {
                                        //     ...config.feature.network,
                                        //     incoming: {
                                        //       ...config.feature.network
                                        //         .incoming,
                                        //       httpFilter: newFilters,
                                        //     },
                                        //   },
                                        // });
                                      }}
                                    >
                                      <Trash2 className="h-4 w-4" />
                                    </Button>
                                  </div>
                                </div>
                              )
                            )}
                          </div>
                        )}
                      </div>

                      {/* Path Filtering */}
                      <div className="space-y-2">
                        <div className="flex items-center justify-between">
                          <Label className="font-medium">Path Filters</Label>
                          <Button
                            type="button"
                            variant="outline"
                            size="sm"
                            onClick={() =>
                              setConfig({
                                ...config,
                                network: {
                                  ...config.feature.network,
                                  incoming: {
                                    ...config.feature.network.incoming,
                                    httpFilter: [
                                      ...config.feature.network.incoming
                                        .httpFilter,
                                      {
                                        type: "path",
                                        value: "",
                                      },
                                    ],
                                  },
                                },
                              })
                            }
                          >
                            <Plus className="h-4 w-4 mr-1" />
                            Add
                          </Button>
                        </div>

                        {(
                          config.feature.network.incoming.httpFilter ?? []
                        ).filter((f) => f.type === "path").length > 0 && (
                          <div className="space-y-2">
                            {(config.feature.network.incoming.httpFilter ?? [])
                              .filter((f) => f.type === "path")
                              .map((filter, index) => (
                                <div
                                  key={index}
                                  className="flex items-center gap-2"
                                >
                                  <Input
                                    placeholder="e.g., /api/v1/test"
                                    value={filter.value}
                                    onChange={(e) => {
                                      const newFilters = [
                                        ...config.feature.network.incoming
                                          .httpFilter,
                                      ];
                                      const pathIndex = newFilters.findIndex(
                                        (f, i) =>
                                          f.type === "path" && i === index
                                      );
                                      if (pathIndex !== -1) {
                                        newFilters[pathIndex] = {
                                          ...newFilters[pathIndex],
                                          value: e.target.value,
                                        };
                                        setConfig({
                                          ...config,
                                          network: {
                                            ...config.feature.network,
                                            incoming: {
                                              ...config.feature.network
                                                .incoming,
                                              httpFilter: newFilters,
                                            },
                                          },
                                        });
                                      }
                                    }}
                                  />
                                  <Button
                                    type="button"
                                    variant="outline"
                                    size="sm"
                                    onClick={() => {
                                      const newFilters = (
                                        config.feature.network.incoming
                                          .httpFilter ?? []
                                      ).filter(
                                        (f, i) =>
                                          !(f.type === "path" && i === index)
                                      );
                                      setConfig({
                                        ...config,
                                        network: {
                                          ...config.feature.network,
                                          incoming: {
                                            ...config.feature.network.incoming,
                                            httpFilter: newFilters,
                                          },
                                        },
                                      });
                                    }}
                                  >
                                    <Trash2 className="h-4 w-4" />
                                  </Button>
                                </div>
                              ))}
                          </div>
                        )}
                      </div>

                      {/* Filter Logic Selection - Only show when there are multiple filters */}
                      {(config.feature.network.incoming.httpFilter ?? [])
                        .length > 1 && (
                        <>
                          <Separator />
                          <div className="space-y-2">
                            <Label className="font-medium">Filter Logic</Label>
                            <RadioGroup
                              value={
                                config.feature.network.incoming.filterOperator
                              }
                              onValueChange={(value: "AND" | "OR") =>
                                setConfig({
                                  ...config,
                                  network: {
                                    ...config.feature.network,
                                    incoming: {
                                      ...config.feature.network.incoming,
                                      filterOperator: value,
                                    },
                                  },
                                })
                              }
                            >
                              <div className="flex items-center space-x-2">
                                <RadioGroupItem value="AND" id="and" />
                                <Label htmlFor="and" className="text-sm">
                                  <strong>All</strong> - Match all specified
                                  filters
                                </Label>
                              </div>
                              <div className="flex items-center space-x-2">
                                <RadioGroupItem value="OR" id="or" />
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
                      />
                    </h3>
                    <p className="text-xs text-muted-foreground mb-3">
                      Click a port to steal or mirror traffic from the remote
                      port (other ports will remain local). Deselect any ports
                      that are not HTTP or gRPC. The following ports were
                      detected in your target, with some already preselected by
                      default.
                    </p>
                  </div>

                  {togglePortsEnabled && (
                    <div className="space-y-4">
                      {/* Port selection from list of detected ports, ports are added to incoming.ports */}
                      <div>
                        <div className="flex flex-wrap gap-2">
                          {mockPorts.map((port) => {
                            const isSelected = readCurrentPorts(config).some(
                              (p) => p[1] === port
                            );
                            const togglePort = () => {
                              if (isSelected) {
                                // Remove port
                                const newPorts = readCurrentPorts(
                                  config
                                ).filter((p) => p !== port);
                                const newConfig = updateConfigPorts(
                                  newPorts,
                                  config
                                );
                                setConfig(newConfig);
                              } else {
                                // Add port
                                const newPorts = readCurrentPorts(config);
                                newPorts.push(port);
                                const newConfig = updateConfigPorts(
                                  newPorts,
                                  config
                                );
                                setConfig(newConfig);
                              }
                            };
                            return (
                              <Button
                                key={port}
                                variant={isSelected ? "default" : "outline"}
                                size="sm"
                                className={`rounded-full px-4 py-2 font-mono transition-all ${
                                  isSelected
                                    ? "bg-primary text-primary-foreground hover:bg-primary/90"
                                    : "hover:bg-primary/10"
                                }`}
                                onClick={togglePort}
                              >
                                {port}
                              </Button>
                            );
                          })}
                        </div>
                      </div>

                      {/* Port Mappings */}
                      {readCurrentPorts(config).length > 0 && (
                        <div className="space-y-3">
                          <h4 className="text-base font-semibold">
                            Selected Ports
                          </h4>
                          <div className="space-y-3">
                            {readCurrentPorts(config).map(
                              (remotePort, index) => (
                                <div
                                  key={remotePort}
                                  className="border rounded-lg p-4 space-y-3"
                                >
                                  <div className="flex items-center justify-between">
                                    <div className="flex items-center gap-2">
                                      <Badge
                                        variant="outline"
                                        className="font-mono"
                                      >
                                        {remotePort}
                                      </Badge>
                                      <span className="text-sm text-muted-foreground">
                                        Remote Port
                                      </span>
                                    </div>
                                  </div>

                                  <div className="flex items-center space-x-2">
                                    <Checkbox
                                      id={`remote-port-${remotePort}`}
                                      checked={
                                        portConfig.local !== portConfig.remote // checked if remotePort in port mappings
                                      }
                                      onCheckedChange={(checked) => {
                                        // const newPorts = [
                                        //   ...config.feature.network.incoming
                                        //     .ports,
                                        // ];
                                        // newPorts[index] = {
                                        //   ...newPorts[index],
                                        //   local: checked
                                        //     ? ""
                                        //     : portConfig.remote,
                                        // };
                                        // setConfig({
                                        //   ...config,
                                        //   network: {
                                        //     ...config.feature.network,
                                        //     incoming: {
                                        //       ...config.feature.network
                                        //         .incoming,
                                        //       ports: newPorts,
                                        //     },
                                        //   },
                                        // });
                                        // TODO: on change, add port to port mapping (mapped to self) or remove port from port mapping
                                      }}
                                    />
                                    <Label
                                      htmlFor={`remote-port-${remotePort}`}
                                      className="text-sm"
                                    >
                                      Local port is different than remote
                                    </Label>
                                  </div>

                                  {portConfig.local !== portConfig.remote && ( // TODO if remote port in portmappings
                                    <div className="flex items-center gap-3">
                                      <div className="flex-1">
                                        <Label className="text-xs text-muted-foreground">
                                          Local Port
                                        </Label>
                                        <Input
                                          type="text"
                                          pattern="[0-9]*"
                                          className="font-mono"
                                          value={localPort}
                                          onChange={(event) => {
                                            const newPorts: PortMapping =
                                              readCurrentPorts(config).map(
                                                ([local, remote]) => {
                                                  if (local === localPort) {
                                                    // safety: the value string is guaranteed to be numerical due to Input pattern="[0-9]*"
                                                    return [
                                                      Number(
                                                        event.target.value
                                                      ),
                                                      remote,
                                                    ];
                                                  } else {
                                                    return [local, remote];
                                                  }
                                                }
                                              );
                                            const newConfig = updateConfigPorts(
                                              newPorts,
                                              config
                                            );
                                            setConfig(newConfig);
                                          }}
                                        />
                                      </div>

                                      <ArrowRight className="h-4 w-4 text-muted-foreground flex-shrink-0 mt-4" />

                                      <div className="flex-1">
                                        <Label className="text-xs text-muted-foreground">
                                          Remote Port
                                        </Label>
                                        <Input
                                          type="text"
                                          pattern="[0-9]*"
                                          className="font-mono"
                                          value={remotePort}
                                          onChange={(event) => {
                                            const newPorts: PortMapping =
                                              readCurrentPortMapping(
                                                config
                                              ).map(([local, remote]) => {
                                                if (remote === remotePort) {
                                                  // safety: the value string is guaranteed to be numerical due to Input pattern="[0-9]*"
                                                  return [
                                                    local,
                                                    Number(event.target.value),
                                                  ];
                                                } else {
                                                  return [local, remote];
                                                }
                                              });
                                            const newConfig =
                                              updateConfigPortMapping(
                                                newPorts,
                                                config
                                              );
                                            setConfig(newConfig);
                                          }}
                                        />
                                      </div>
                                    </div>
                                  )}
                                </div>
                              )
                            )}
                          </div>
                        </div>
                      )}
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
