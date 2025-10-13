import { DefaultConfig } from "./UserDataContext";

// For validating the config JSON is well-formed JSON
export const validateJson = (
  jsonString: string,
  setJsonError: (error: string) => void
): boolean => {
  try {
    JSON.parse(jsonString);
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

// Update some or all of the config.target
export const updateConfigTarget = (
  config: any,
  setConfig: (config: any) => void,
  targetType?: string,
  targetName?: string,
  targetNamespace?: string
) => {
  // TODO
  return;
};

// Update config.feature.network.incoming.mode
export const updateConfigMode = (
  mode: "mirror" | "steal",
  config: any,
  setConfig: (config: any) => void
) => {
  // TODO
  return;
};

// Update config.feature.network.filter
export const updateConfigFilter = (
  headerFilters: string[],
  pathFilters: string[],
  operator: "any" | "all",
  config: any,
  setConfig: (config: any) => void
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
    config.feature.network = DefaultConfig.feature.network;
  }

  if (
    !("incoming" in config.feature.network) ||
    typeof config.feature.network.incoming !== "object"
  ) {
    config.feature.network.incoming = DefaultConfig.feature.network.incoming;
  }

  if (
    !("http_filter" in config.feature.network.incoming) ||
    typeof config.feature.network.incoming.http_filter !== "object"
  ) {
    config.feature.network.incoming.http_filter =
      DefaultConfig.feature.network.incoming.http_filter;
  }

  // create new value for config.feature.network.incoming.http_filter
  let http_filter;
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
    network: {
      ...config.network,
      incoming: {
        ...config.network.incoming,
        http_filter: http_filter,
      },
    },
  };

  setConfig(newConfig);
  return;
};

// Update config.feature.network.ports
export const updateConfigPorts = (
  portMappings: any[],
  config: any,
  setConfig: (config: any) => void
) => {
  // TODO
  return;
};

// TODO: ??? turn into updateConfigDirectly()
export const updateConfigFromJson = (
  jsonString: string,
  setConfig: React.Dispatch<React.SetStateAction<any>>,
  setJsonError: (error: string) => void
): void => {
  if (validateJson(jsonString, setJsonError)) {
    try {
      const parsedConfig = JSON.parse(jsonString);
      setConfig((prevConfig) => ({
        ...prevConfig,
        target: parsedConfig.target || prevConfig.target,
        agent: {
          scaledown: parsedConfig.agent?.scaledown || false,
          copyTarget: parsedConfig.agent?.copy_target || false,
        },
      }));
    } catch (error) {
      console.error("Error updating config from JSON:", error);
    }
  }
};

