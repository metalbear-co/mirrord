import {
  FeatureCopyTargetCopyTarget,
  HTTPFilter,
  HttpFilterFileConfig,
  IncomingAdvancedSetup,
  IncomingMode,
  InnerFilter,
  LayerFileConfig,
  NetworkFileConfig,
  PortMapping,
  Target,
  TargetFileConfig,
  ToggleableConfigFor_IncomingFileConfig,
} from "@/mirrord-schema";
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

// Infer the selected boilerplate type (or `custom` if the config has been changed manually)
// from config state.
export const readBoilerplateType = (
  config: LayerFileConfig
): "steal" | "mirror" | "replace" | "custom" => {
  if (
    typeof config.feature.network === "object" &&
    typeof config.feature.network.incoming === "object"
  ) {
    if (config.feature.network.incoming.mode === "mirror") {
      return "mirror";
    }
    if (typeof config.feature.copy_target === "object") {
      if (
        config.feature.copy_target.enabled &&
        config.feature.copy_target.scale_down
      ) {
        return "replace";
      } else if (
        !config.feature.copy_target.enabled &&
        !config.feature.copy_target.scale_down
      ) {
        return "steal";
      }
    }
  }
  return "custom";
};

const getTargetDetails = (
  target: Target | any
): { type: string; name?: string } => {
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
  config: LayerFileConfig
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

// Return the filters currently set in the given config, as well as the operator used to
// combine them ("any", "all" or null).
// Instead of using the generated type `InnerFilter`, return a list of strings for header and path.
export const readCurrentFilters = (
  config: LayerFileConfig
): {
  header: string[];
  path: string[];
  operator: "any" | "all" | null;
} => {
  let headerGenType: InnerFilter[] = [];
  let pathGenType: InnerFilter[] = [];
  let operator = null;

  if (
    typeof config.feature?.network === "object" &&
    typeof config.feature?.network.incoming === "object" &&
    typeof config.feature?.network.incoming.http_filter === "object"
  ) {
    const filter = config.feature?.network.incoming.http_filter;

    if (filter.header_filter) {
      // single header filter
      headerGenType = [{ header: filter.header_filter }];
    } else if (filter.path_filter) {
      // single path filter
      pathGenType = [{ path: filter.path_filter }];
    } else if (filter.all_of || filter.any_of) {
      // multiple filters
      headerGenType = filter.all_of
        ?.filter((innerFilter) => {
          "header" in innerFilter;
        })
        .concat(
          filter.any_of?.filter((innerFilter) => {
            "header" in innerFilter;
          })
        ) ?? [];
      pathGenType = filter.all_of
        ?.filter((innerFilter) => {
          "path" in innerFilter;
        })
        .concat(
          filter.any_of?.filter((innerFilter) => {
            "path" in innerFilter;
          })
        ) ?? [];

      if (filter.all_of) operator = "all";
      else if (filter.any_of) operator = "any";
    }
  }

  // ### Generated types (they may change slightly as mirrord config changes):
  //
  // export type InnerFilter =
  //   | Feature[...]HeaderFilter
  //   | Feature[...]PathFilter
  //   | {
  //       method: string;
  //       [k: string]: unknown;
  //     };
  //
  // export interface Feature[...]HeaderFilter {
  //   header: string;
  //   [k: string]: unknown;
  // }
  //
  // export interface Feature[...]PathFilter {
  //   path: string;
  //   [k: string]: unknown;
  // }
  //
  // For this, we can ignore the unknown keys and method filtering.

  const header: string[] = headerGenType
    .filter((inner) => "header" in inner)
    .map((inner) => (inner.header as string) ?? "")
    .filter((string) => string.length > 0);
  const path: string[] = pathGenType
    .filter((inner) => "path" in inner)
    .map((inner) => (inner.path as string) ?? "")
    .filter((string) => string.length > 0);
  return { header, path, operator };
};

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

export const readCurrentPortMapping = (
  config: LayerFileConfig
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

export const readIncoming = (
  config: LayerFileConfig
): ToggleableConfigFor_IncomingFileConfig => {
  if (typeof config.feature?.network === "object") {
    return config.feature?.network.incoming;
  }

  return false;
};

export const updateIncoming = (
  config: LayerFileConfig,
  newIncoming: ToggleableConfigFor_IncomingFileConfig
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

// Returns an updated config with new config.target according to parameters.
export const updateConfigTarget = (
  config: LayerFileConfig,
  target: string,
  targetNamespace: string
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

// Returns an updated config with new config.feature.network.incoming.mode according to parameters.
export const updateConfigMode = (
  mode: "mirror" | "steal",
  config: LayerFileConfig
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

// Returns an updated config with new config.feature.network.incoming.mode according to parameters.
export const updateConfigCopyTarget = (
  copy_target: boolean,
  scale_down: boolean,
  config: LayerFileConfig
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

// Sets filters to null
// export const turnOffConfigFilter = (config: LayerFileConfig) => {
//   if (
//     typeof config.feature?.network === "object" &&
//     typeof config.feature?.network.incoming === "object"
//   ) {
//     const newIncoming = {
//       ...config.feature.network.incoming,
//       http_filter: null,
//     };
//     return updateIncoming(config, newIncoming);
//   }

//   return config;
// };

// Returns an updated config with new config.feature.network.filter according to parameters.
export const updateConfigFilter = (
  filters: {
    headerFilters: string[];
    pathFilters: string[];
    operator: "any" | "all";
  },
  config: LayerFileConfig
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
    !("http_filter" in config.feature.network.incoming) ||
    typeof config.feature.network.incoming.http_filter !== "object"
  ) {
    config.feature.network.incoming.http_filter = (
      (DefaultConfig.feature.network as NetworkFileConfig)
        .incoming as IncomingAdvancedSetup
    ).http_filter;
  }

  // create new value for config.feature.network.incoming.http_filter
  let http_filter: HTTPFilter;
  switch (filters.operator) {
    case "any":
      http_filter = {
        any_of: filters.headerFilters
          .map((headerFilter) => {
            return { header: headerFilter } as any;
          })
          .concat(
            filters.pathFilters.map((pathFilter) => {
              return { path: pathFilter };
            })
          ),
      };
      break;
    case "all":
      http_filter = {
        all_of: filters.headerFilters
          .map((headerFilter) => {
            return { header: headerFilter } as any;
          })
          .concat(
            filters.pathFilters.map((pathFilter) => {
              return { path: pathFilter };
            })
          ),
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

  // overwrite ports
  const newConfig = {
    ...config,
    feature: {
      ...config.feature,
      network: {
        ...config.feature.network,
        incoming: {
          ...config.feature.network.incoming,
          ports: ports,
        },
      },
    },
  };

  return newConfig;
};

// Returns an updated config with new config.feature.network.incoming.port_mapping according
// to parameters.
export const updateConfigPortMapping = (
  portMappings: PortMapping,
  config: LayerFileConfig
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

// Update config from user's text box input, if and only if it passes validation.
export const updateConfigFromJson = (
  jsonString: string,
  setJsonError: (error: string) => void
): void => {
  if (validateJson(jsonString, setJsonError)) {
    try {
      const parsedConfig = JSON.parse(jsonString);
      // setConfig(parsedConfig); // TODO:?
    } catch (error) {
      console.error("Error updating config from JSON:", error);
    }
  }
};

// For validating the config JSON is well-formed JSON.
export const validateJson = (
  jsonString: string,
  setJsonError: (error: string) => void
): boolean => {
  let wellFormedJson;
  try {
    wellFormedJson = JSON.parse(jsonString);
    setJsonError("");
    return true;
  } catch (error) {
    setJsonError(
      `Invalid JSON: ${
        error instanceof Error ? error.message : "Unknown error"
      }`
    );
    return false;
  }
};

// Stringify the config object with whitespace for display or file download
export const getConfigString = (config: LayerFileConfig): string => {
  return JSON.stringify(config, null, 2);
};
