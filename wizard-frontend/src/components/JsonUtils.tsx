import {
  FeatureCopyTargetCopyTarget,
  HTTPFilter,
  IncomingAdvancedSetup,
  IncomingMode,
  LayerFileConfig,
  NetworkFileConfig,
  PortMapping,
} from "@/mirrord-schema";
import { DefaultConfig } from "./UserDataContext";

// infer the selected boilerplate type (or `custom` if the config has been changed manually) from config state
export const readBoilerplateType = (config: LayerFileConfig): "steal" | "mirror" | "replace" | "custom" => {
  // TODO

  // if (mode === "mirror") {
  //     return "Mirror mode"
  //   }
  //   if (config.config.agent.scaledown && config.config.agent.copyTarget) {
  //     return "Replace mode";
  //   } else if (!config.config.agent.scaledown && !config.config.agent.copyTarget) {
  //     return "Filtering mode";
  //   }
  //   return "Custom mode";
  
  return "steal";
}

// Update config.target
export const updateConfigTarget = (
  config: LayerFileConfig,
  setConfig: (config: LayerFileConfig) => void,
  targetType: string,
  targetName: string,
  targetNamespace: string
) => {
  // create new value for target
  let targetPath;
  if (targetType === "targetless") {
    targetPath = targetType;
  } else {
    targetPath = targetType + "/" + targetName;
  }

  const newTarget = {
    path: targetPath,
    namespace: targetNamespace,
  };

  // overwrite target
  const newConfig = {
    ...config,
    target: newTarget,
  };

  setConfig(newConfig);
};

// Update config.feature.network.incoming.mode
export const updateConfigMode = (
  mode: "mirror" | "steal",
  config: LayerFileConfig,
  setConfig: (config: LayerFileConfig) => void
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

  setConfig(newConfig);
};

// Update config.feature.network.incoming.mode
export const updateConfigCopyTarget = (
  copy_target: boolean,
  scale_down: boolean,
  config: LayerFileConfig,
  setConfig: (config: LayerFileConfig) => void
) => {
  if (typeof config !== "object") {
    throw "config badly formed";
  }

  // If config is not in the right shape, insert from default config
  if (!("feature" in config) || typeof config.feature !== "object") {
    config.feature = DefaultConfig.feature;
  }

  // overwrite copy_target
  const newConfig = {
    ...config,
    feature: {
      ...config.feature,
      copy_target: {
        enabled: copy_target,
        scale_down: scale_down
      }
    },
  };

  setConfig(newConfig);
};

// Update config.feature.network.filter
export const updateConfigFilter = (
  headerFilters: string[],
  pathFilters: string[],
  operator: "any" | "all",
  config: LayerFileConfig,
  setConfig: (config: LayerFileConfig) => void
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
  switch (operator) {
    case "any":
      http_filter = {
        any_of: headerFilters
          .map((headerFilter) => {
            return { header: headerFilter } as any;
          })
          .concat(
            pathFilters.map((pathFilter) => {
              return { path: pathFilter };
            })
          ),
      };
      break;
    case "all":
      http_filter = {
        all_of: headerFilters
          .map((headerFilter) => {
            return { header: headerFilter } as any;
          })
          .concat(
            pathFilters.map((pathFilter) => {
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

  setConfig(newConfig);
};

// Update config.feature.network.incoming.port_mapping
export const updateConfigPorts = (
  portMappings: PortMapping,
  config: LayerFileConfig,
  setConfig: (config: LayerFileConfig) => void
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

  // create new value for config.feature.network.incoming.port_mapping
  const newMappings = portMappings;

  // overwrite ports
  const newConfig = {
    ...config,
    feature: {
      ...config.feature,
      network: {
        ...config.feature.network,
        incoming: {
          ...config.feature.network.incoming,
          port_mapping: newMappings,
        },
      },
    },
  };

  setConfig(newConfig);
};

// Update config from user's text box input, if and only if it passes validation
export const updateConfigFromJson = (
  jsonString: string,
  setConfig: (config: LayerFileConfig) => void,
  setJsonError: (error: string) => void
): void => {
  if (validateJson(jsonString, setJsonError)) {
    try {
      const parsedConfig = JSON.parse(jsonString);
      setConfig(parsedConfig);
    } catch (error) {
      console.error("Error updating config from JSON:", error);
    }
  }
};

// For validating the config JSON is well-formed JSON
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
