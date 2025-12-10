import type {
  HTTPFilter,
  IncomingAdvancedSetup,
  IncomingMode,
  InnerFilter,
  LayerFileConfig,
  NetworkFileConfig,
  PortMapping,
  Target,
  ToggleableConfigFor_IncomingFileConfig,
} from "../mirrord-schema";
import { DefaultConfig } from "./UserDataContext";

// These functions are utils to interact with the current config.
// The config type `LayerFileConfig` is generated from the mirrord schema file, as are all
// the other config-related types. These are imported from `src/mirrord-schema.d.ts`.
//
// The functions here are all one of two types:
// - functions that read the current state of the config
// - functions that create a new config from the existing one, with some specific section changed
//
// *NOTE* that functions of the latter type DO NOT update the config directly, they return a new
// `LayerFileConfig` which must be set _by the caller_.

// ===== BOILERPLATE: MODE AND COPY TARGET =====

// Infer the selected boilerplate type (or `custom` if the config has been changed manually)
// from config state.
export const readBoilerplateType = (
  config: LayerFileConfig,
): "steal" | "mirror" | "replace" | "custom" => {
  if (
    typeof config.feature?.network === "object" &&
    typeof config.feature.network?.incoming === "object"
  ) {
    if (config.feature.network.incoming?.mode === "mirror") {
      return "mirror";
    }
    if (typeof config.feature.copy_target === "object") {
      if (
        config.feature.copy_target?.enabled &&
        config.feature.copy_target.scale_down
      ) {
        return "replace";
      } else if (
        !config.feature.copy_target?.enabled &&
        !config.feature.copy_target?.scale_down
      ) {
        return "steal";
      }
    }
  }
  return "custom";
};

// Return an updated config with updated incoming.mode.
export const updateConfigMode = (
  mode: "mirror" | "steal",
  config: LayerFileConfig,
) => {
  if (typeof config !== "object") {
    throw "config badly formed";
  }

  // If config is not in the right shape, insert from default config
  if (!("feature" in config) || typeof config.feature !== "object") {
    config.feature = DefaultConfig.feature;
  }

  if (
    !("network" in config.feature) ||
    typeof config.feature.network !== "object"
  ) {
    config.feature.network = DefaultConfig.feature.network as NetworkFileConfig;
  }

  // type IncomingNetwork = (IncomingMode | null) | IncomingAdvancedSetup;
  if (
    !("incoming" in config.feature.network) ||
    typeof config.feature.network.incoming !== "object"
  ) {
    if (typeof config.feature.network.incoming === "string") {
      // incoming is using the simplified IncomingMode, replace with equivalent IncomingAdvancedSetup
      config.feature.network.incoming = {
        mode: config.feature.network.incoming,
      } as IncomingAdvancedSetup;
    } else {
      config.feature.network.incoming = (
        DefaultConfig.feature.network as NetworkFileConfig
      ).incoming as IncomingAdvancedSetup;
    }
  }

  if (
    !("mode" in config.feature.network.incoming) ||
    typeof config.feature.network.incoming.mode !== "string"
  ) {
    config.feature.network.incoming.mode = (
      (DefaultConfig.feature.network as NetworkFileConfig)
        .incoming as IncomingAdvancedSetup
    ).mode;
  }

  // create new value for config.feature.network.incoming.mode
  const newMode = mode as IncomingMode;

  // overwrite mode
  const newConfig = {
    ...config,
    feature: {
      ...config.feature,
      network: {
        ...config.feature.network,
        incoming: {
          ...config.feature.network.incoming,
          mode: newMode,
        },
      },
    },
  };

  return newConfig;
};

// Return an updated config with updated feature.copy_target.
export const updateConfigCopyTarget = (
  copy_target: boolean,
  scale_down: boolean,
  config: LayerFileConfig,
) => {
  if (typeof config !== "object") {
    throw "config badly formed";
  }

  // If config is not in the right shape, insert from default config
  if (!("feature" in config) || typeof config.feature !== "object") {
    config.feature = DefaultConfig.feature;
  }

  // overwrite copy_target
  const newConfig: LayerFileConfig = {
    ...config,
    feature: {
      ...config.feature,
      copy_target: {
        enabled: copy_target,
        scale_down: scale_down,
      },
    },
  };

  return newConfig;
};

// ===== TARGET =====

// Extract the (resource) type and name of a target from the generated Target type
const getTargetDetails = (target: Target): { type: string; name?: string } => {
  if (target === "targetless") {
    return { type: "targetless" };
  }
  if (typeof target === "string") {
    const nameParts = target.split("/");
    if (nameParts.length < 2) {
      return { type: "targetless" };
    } else {
      return { type: nameParts[0], name: nameParts[1] };
    }
  }
  if ("deployment" in target) {
    return { type: "deployment", name: target.deployment };
  }
  if ("pod" in target) {
    return { type: "pod", name: target.pod };
  }
  if ("rollout" in target) {
    return { type: "rollout", name: target.rollout };
  }
  if ("job" in target) {
    return { type: "job", name: target.job };
  }
  if ("cron_job" in target) {
    return { type: "cron_job", name: target.cron_job };
  }
  if ("stateful_set" in target) {
    return { type: "stateful_set", name: target.stateful_set };
  }
  if ("service" in target) {
    return { type: "service", name: target.service };
  }
  if ("replica_set" in target) {
    return { type: "replica_set", name: target.replica_set };
  }
  return { type: "targetless" };
};

// Return the type and name of the target currently set in the given config.
export const readCurrentTargetDetails = (
  config: LayerFileConfig,
): { type: string; name?: string } => {
  const target = config.target;
  if (typeof target === "string") {
    const nameParts = target.split("/");
    if (nameParts.length < 2) {
      return { type: "targetless" };
    } else {
      return { type: nameParts[0], name: nameParts[1] };
    }
  } else if (!target) {
    return { type: "targetless" };
  } else if (typeof target === "object") {
    if ("path" in target) {
      return getTargetDetails(target.path);
    } else {
      return getTargetDetails(target);
    }
  }
};

// Return an updated config with updated config.target.
export const updateConfigTarget = (
  config: LayerFileConfig,
  target: string,
  targetNamespace: string,
) => {
  const newTarget = {
    path: target,
    namespace: targetNamespace,
  };

  // overwrite target
  const newConfig = {
    ...config,
    target: newTarget,
  };

  return newConfig;
};

// ===== INCOMING NETWORK =====

// Return the entire `network.incoming` section of config.
export const readIncoming = (
  config: LayerFileConfig,
): ToggleableConfigFor_IncomingFileConfig => {
  if (typeof config.feature?.network === "object") {
    return config.feature?.network.incoming;
  }

  return false;
};

// set the entire `network.incoming` section of config.
export const updateIncoming = (
  config: LayerFileConfig,
  newIncoming: ToggleableConfigFor_IncomingFileConfig,
) => {
  if (typeof config !== "object") {
    throw "config badly formed";
  }

  // If config is not in the right shape, insert from default config
  if (!("feature" in config) || typeof config.feature !== "object") {
    config.feature = DefaultConfig.feature;
  }

  if (
    !("network" in config.feature) ||
    typeof config.feature.network !== "object"
  ) {
    config.feature.network = DefaultConfig.feature.network as NetworkFileConfig;
  }

  const newConfig = {
    ...config,
    feature: {
      ...config.feature,
      network: {
        ...config.feature.network,
        incoming: newIncoming,
      },
    },
  };

  return newConfig;
};

// ===== FILTERS =====

// A helpful type for handling filters in the UI to avoid using the generated type.
// IMPORTANT: NEVER STORE EXACT MATCH FILTERS IN CONFIG, always convert them first
export interface UiHttpFilter {
  value: string;
  type: "header" | "path";
}

// Return the filters currently set in the given config, as well as the operator used to
// combine them ("any", "all" or null).
// Instead of using the generated type `InnerFilter`, uses `UiHttpFilter`.
export const readCurrentFilters = (
  config: LayerFileConfig,
): {
  filters: UiHttpFilter[];
  operator: "any" | "all" | null;
} => {
  let filters: UiHttpFilter[] = [];
  let operator = null;

  if (
    typeof config.feature?.network === "object" &&
    typeof config.feature?.network.incoming === "object" &&
    typeof config.feature?.network.incoming.http_filter === "object" &&
    config.feature?.network.incoming.http_filter
  ) {
    const filterConfig = config.feature?.network.incoming.http_filter;

    if ("header" in filterConfig && typeof filterConfig.header === "string") {
      // single header filter
      filters = [
        {
          value: filterConfig.header,
          type: "header",
        },
      ];
    } else if (
      "path" in filterConfig &&
      typeof filterConfig.path === "string"
    ) {
      // single path filter
      filters = [{ value: filterConfig.path, type: "path" }];
    } else if ("all_of" in filterConfig) {
      // multiple filters
      operator = "all";
      filters = filterConfig.all_of?.map((filter) => {
        let value: string;
        let filterType: "header" | "path";
        if ("header" in filter) {
          filterType = "header";
          if (typeof filter.header === "string") value = filter.header;
        } else if ("path" in filter) {
          filterType = "path";
          if (typeof filter.path === "string") value = filter.path;
        }
        if (filterType && value) {
          return { value: value, type: filterType };
        }
      });
    } else if ("any_of" in filterConfig) {
      // multiple filters
      operator = "any";
      filters = filterConfig.any_of?.map((filter) => {
        let value: string;
        let filterType: "header" | "path";
        if ("header" in filter) {
          filterType = "header";
          if (typeof filter.header === "string") value = filter.header;
          else value = "";
        } else if ("path" in filter) {
          filterType = "path";
          if (typeof filter.path === "string") value = filter.path;
          else value = "";
        }
        if (filterType && value) {
          return { value: value, type: filterType };
        }
      });
    }
    filters = filters.filter((filter) => filter.value?.length > 0);
  }
  return { filters, operator };
};

// Return an updated config with filters set to null.
export const disableConfigFilter = (config: LayerFileConfig) => {
  if (
    typeof config.feature?.network === "object" &&
    typeof config.feature?.network.incoming === "object"
  ) {
    delete config.feature?.network.incoming.http_filter;
  }
  return config;
};

// Return an updated config with updated incoming.http_filter and, iff ports are set,
// incoming.ports.
export const updateConfigFilter = (
  filters: UiHttpFilter[],
  operator: "any" | "all" | null,
  config: LayerFileConfig,
) => {
  console.log(filters); // FIX get correct filters, setting config fails
  if (typeof config !== "object") {
    throw "config badly formed";
  }

  // If config is not in the right shape, insert from default config
  if (!("feature" in config) || typeof config.feature !== "object") {
    config.feature = DefaultConfig.feature;
  }

  if (
    !("network" in config.feature) ||
    typeof config.feature.network !== "object"
  ) {
    config.feature.network = DefaultConfig.feature.network as NetworkFileConfig;
  }

  // type IncomingNetwork = (IncomingMode | null) | IncomingAdvancedSetup;
  if (
    !("incoming" in config.feature.network) ||
    typeof config.feature.network.incoming !== "object"
  ) {
    if (typeof config.feature.network.incoming === "string") {
      // incoming is using the simplified IncomingMode, replace with equivalent IncomingAdvancedSetup
      config.feature.network.incoming = {
        mode: config.feature.network.incoming,
      } as IncomingAdvancedSetup;
    } else {
      config.feature.network.incoming = (
        DefaultConfig.feature.network as NetworkFileConfig
      ).incoming as IncomingAdvancedSetup;
    }
  }

  // create new value for config.feature.network.incoming.http_filter
  let http_filter: HTTPFilter;
  if (filters.length === 0) {
    // filters config toggled on, but no filters set
    http_filter = null;
  } else if (filters.length === 1) {
    // single filter, ignore operator
    const filter = filters[0];
    if (filter.type === "header") {
      http_filter = { header: filter.value } as InnerFilter;
    } else if (filter.type === "path") {
      http_filter = { path: filter.value } as InnerFilter;
    } else {
      http_filter = null;
    }
  } else {
    switch (operator) {
      case null:
      case "any":
        http_filter = {
          any_of: filters.map((filter) => {
            if (filter.type === "header") {
              return { header: filter.value } as InnerFilter;
            } else if (filter.type === "path") {
              return { path: filter.value } as InnerFilter;
            } else {
              return;
            }
          }),
        };
        break;
      case "all":
        http_filter = {
          all_of: filters.map((filter) => {
            if (filter.type === "header") {
              return { header: filter.value } as InnerFilter;
            } else if (filter.type === "path") {
              return { path: filter.value } as InnerFilter;
            } else {
              return;
            }
          }),
        };
        break;
    }
  }

  // update ports if incoming.ports is set and http_filter is not null
  if (
    "ports" in config.feature.network.incoming &&
    typeof http_filter === "object" &&
    http_filter
  ) {
    http_filter = {
      ...http_filter,
      ports: readCurrentPorts(config),
    };
  }

  // overwrite filter
  const newConfig = {
    ...config,
    feature: {
      ...config.feature,
      network: {
        ...config.feature.network,
        incoming: {
          ...config.feature.network.incoming,
          http_filter: http_filter,
        },
      },
    },
  };

  return newConfig;
};

export const updateSingleFilter = (
  oldFilter: UiHttpFilter,
  updatedFilter: UiHttpFilter,
  config: LayerFileConfig,
): LayerFileConfig => {
  const { filters, operator } = readCurrentFilters(config);
  const newFilters = filters
    .filter(
      (filter) =>
        !(filter.type === oldFilter.type && filter.value === oldFilter.value),
    )
    .concat([updatedFilter]);

  return updateConfigFilter(newFilters, operator, config);
};

export const removeSingleFilter = (
  removedFilter: UiHttpFilter,
  config: LayerFileConfig,
): LayerFileConfig => {
  const { filters, operator } = readCurrentFilters(config);
  const newFilters = filters.filter(
    (filter) =>
      !(
        filter.type === removedFilter.type &&
        filter.value === removedFilter.value
      ),
  );

  return updateConfigFilter(newFilters, operator, config);
};

// Return the regex expression for an exact match of the given string value.
export const regexificationRay = (value: string) =>
  "^" + value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&") + "$";

// ===== PORTS AND PORT MAPPING(S) =====

// Return the ports currently set to remote from `incoming.ports`.
// Separate and distinct from `incoming.port_mapping`.
export const readCurrentPorts = (config: LayerFileConfig): number[] => {
  if (
    typeof config.feature?.network === "object" &&
    typeof config.feature?.network.incoming === "object" &&
    config.feature?.network.incoming.ports
  ) {
    return config.feature?.network.incoming.ports;
  }

  return [];
};

// Return the port maps currently set in `incoming.port_mapping`.
export const readCurrentPortMapping = (
  config: LayerFileConfig,
): PortMapping => {
  if (
    typeof config.feature?.network === "object" &&
    typeof config.feature?.network.incoming === "object" &&
    config.feature?.network.incoming.port_mapping
  ) {
    return config.feature?.network.incoming.port_mapping;
  }

  return [];
};

// Return an updated config with ports and port_mapping set to null.
export const disablePortsAndMapping = (config: LayerFileConfig) => {
  if (
    typeof config.feature?.network === "object" &&
    typeof config.feature?.network.incoming === "object"
  ) {
    delete config.feature?.network.incoming.ports;
    delete config.feature?.network.incoming.port_mapping;
  }
  return config;
};

// Return an updated config with updated incoming.ports and, iff filters are set, http_filter.ports.
export const updateConfigPorts = (ports: number[], config: LayerFileConfig) => {
  if (typeof config !== "object") {
    throw "config badly formed";
  }

  // If config is not in the right shape, insert from default config
  if (!("feature" in config) || typeof config.feature !== "object") {
    config.feature = DefaultConfig.feature;
  }

  if (
    !("network" in config.feature) ||
    typeof config.feature.network !== "object"
  ) {
    config.feature.network = DefaultConfig.feature.network as NetworkFileConfig;
  }

  // type IncomingNetwork = (IncomingMode | null) | IncomingAdvancedSetup;
  if (
    !("incoming" in config.feature.network) ||
    typeof config.feature.network.incoming !== "object"
  ) {
    if (typeof config.feature.network.incoming === "string") {
      // incoming is using the simplified IncomingMode, replace with equivalent IncomingAdvancedSetup
      config.feature.network.incoming = {
        mode: config.feature.network.incoming,
      } as IncomingAdvancedSetup;
    } else {
      config.feature.network.incoming = (
        DefaultConfig.feature.network as NetworkFileConfig
      ).incoming as IncomingAdvancedSetup;
    }
  }

  // no ports in default config, so insert blank list instead
  if (
    !("ports" in config.feature.network.incoming) ||
    !config.feature.network.incoming.ports
  ) {
    config.feature.network.incoming.ports = [];
  }

  let incomingConfig: IncomingAdvancedSetup = {
    ...config.feature.network.incoming,
    ports: ports,
  };

  // if filters are set, set ports into http_filter as well
  if (
    "http_filter" in config.feature.network.incoming &&
    config.feature.network.incoming.http_filter &&
    typeof config.feature.network.incoming.http_filter === "object"
  ) {
    config.feature.network.incoming.http_filter.ports = [];
    incomingConfig = {
      ...incomingConfig,
      http_filter: {
        ...config.feature.network.incoming.http_filter,
        ports: ports,
      },
    };
  }

  // overwrite ports
  const newConfig = {
    ...config,
    feature: {
      ...config.feature,
      network: {
        ...config.feature.network,
        incoming: incomingConfig,
      },
    },
  };

  return newConfig;
};

// Return an updated config with updated incoming.port_mapping.
export const updateConfigPortMapping = (
  portMappings: PortMapping,
  config: LayerFileConfig,
) => {
  if (typeof config !== "object") {
    throw "config badly formed";
  }

  // If config is not in the right shape, insert from default config
  if (!("feature" in config) || typeof config.feature !== "object") {
    config.feature = DefaultConfig.feature;
  }

  if (
    !("network" in config.feature) ||
    typeof config.feature.network !== "object"
  ) {
    config.feature.network = DefaultConfig.feature.network as NetworkFileConfig;
  }

  // type IncomingNetwork = (IncomingMode | null) | IncomingAdvancedSetup;
  if (
    !("incoming" in config.feature.network) ||
    typeof config.feature.network.incoming !== "object"
  ) {
    if (typeof config.feature.network.incoming === "string") {
      // incoming is using the simplified IncomingMode, replace with equivalent IncomingAdvancedSetup
      config.feature.network.incoming = {
        mode: config.feature.network.incoming,
      } as IncomingAdvancedSetup;
    } else {
      config.feature.network.incoming = (
        DefaultConfig.feature.network as NetworkFileConfig
      ).incoming as IncomingAdvancedSetup;
    }
  }

  // no port mapping in default config, so insert blank list instead
  if (
    !("port_mapping" in config.feature.network.incoming) ||
    !config.feature.network.incoming.port_mapping
  ) {
    config.feature.network.incoming.port_mapping = [];
  }

  // overwrite ports
  const newConfig = {
    ...config,
    feature: {
      ...config.feature,
      network: {
        ...config.feature.network,
        incoming: {
          ...config.feature.network.incoming,
          port_mapping: portMappings,
        },
      },
    },
  };

  return newConfig;
};

// Return an updated config with the given port mapping applied:
// If a mapping does not exist for the remote port, create one. Otherwise, remove or update the
// mapping according to the local port.
export const addRemoveOrUpdateMapping = (
  remotePort: number,
  localPort: number,
  config: LayerFileConfig,
) => {
  const existingMappings = readCurrentPortMapping(config);
  const existingMapping = existingMappings.find(
    ([, remote]) => remote === remotePort,
  );

  if (existingMapping) {
    // a mapping exists for this remote, so replace or remove it
    const newMappings = existingMappings.filter(
      (mapping) => mapping !== existingMapping,
    );
    if (remotePort === localPort) {
      // remove existing mapping only
      return updateConfigPortMapping(newMappings, config);
    } else {
      // add new mapping
      return updateConfigPortMapping(
        newMappings.concat([[localPort, remotePort]]),
        config,
      );
    }
  } else {
    if (remotePort === localPort) {
      // no mapping exists, no mapping should exist: do nothing
      return config;
    } else {
      // add new mapping
      return updateConfigPortMapping(
        existingMappings.concat([[localPort, remotePort]]),
        config,
      );
    }
  }
};

// Read mapping for given remote port if it exists and return the local port, otherwise return
// remote port.
export const getLocalPort = (
  remotePort: number,
  config: LayerFileConfig,
): number => {
  const existingMapping = readCurrentPortMapping(config).filter(
    ([, remote]) => remote === remotePort,
  );
  if (existingMapping.length > 0) {
    return existingMapping[0][0];
  } else {
    return remotePort;
  }
};

// Return an updated config with the remote port removed from incoming.ports and its associated
// mapping removed from incoming.port_mapping (if it exists) from config.
export const removePortandMapping = (
  remotePort: number,
  config: LayerFileConfig,
): LayerFileConfig => {
  // remove port
  const ports = readCurrentPorts(config);
  if (ports.includes(remotePort)) {
    // remove the mapping _before_ removing the port, if there is one
    const newPorts = ports.filter((port) => port !== remotePort);

    // remove mapping
    const oldMapping = readCurrentPortMapping(config);
    const matchingMapping = oldMapping.find(
      ([, remote]) => remote === remotePort,
    );

    let halfConfig = config;
    if (matchingMapping) {
      const newMapping = oldMapping.filter(
        (mapping) => mapping !== matchingMapping,
      );
      halfConfig = updateConfigPortMapping(newMapping, config);
    }

    return updateConfigPorts(newPorts, halfConfig);
  } else {
    return config;
  }
};

// ===== JSON HANDLING =====

// Stringify the config object with whitespace for display or file download
export const getConfigString = (config: LayerFileConfig): string => {
  if (readBoilerplateType(config) === "replace") {
    // remove incoming.ports when in replace mode
    const newConfig = updateConfigPorts([], config);
    if (
      typeof newConfig.feature?.network === "object" &&
      typeof newConfig.feature?.network.incoming === "object" &&
      newConfig.feature?.network.incoming.ports
    ) {
      delete newConfig.feature?.network.incoming.ports;
    }
    return JSON.stringify(newConfig, null, 2);
  }
  return JSON.stringify(config, null, 2);
};
