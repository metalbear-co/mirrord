import type { FeatureConfig, ConfigData } from "@/types/config";

export const generateConfigJson = (config: ConfigData): string => {
  const configObj: {
    target?: string;
    agent: {
      copy_target?: boolean;
      scaledown?: boolean;
    };
    feature: FeatureConfig;
  } = {
    target: config.target ? `${config.target}` : undefined,
    agent: {},
    feature: {},
  };
  
  if (config.agent.copyTarget) configObj.agent.copy_target = true;
  if (config.agent.scaledown) configObj.agent.scaledown = true;
  
  if (config.network.incoming.enabled) {
    configObj.feature.network = {
      incoming: {
        mode: config.network.incoming.mode,
      },
    };
    if (config.network.incoming.httpFilter.length > 0) {
      configObj.feature.network.incoming.http_filter = {
        [config.network.incoming.filterOperator.toLowerCase()]:
          config.network.incoming.httpFilter.map((f) => ({
            [f.type]: f.value,
          })),
      };
    }
    if (config.network.incoming.ports.length > 0) {
      configObj.feature.network.incoming.ports =
        config.network.incoming.ports.map((p) => ({
          [p.remote]: p.local,
        }));
    }
  }
  
  if (config.network.outgoing.enabled) {
    if (!configObj.feature.network) configObj.feature.network = {};
    configObj.feature.network.outgoing = {
      filter: {
        [config.network.outgoing.filterTarget]:
          config.network.outgoing.protocol !== "both"
            ? {
                [config.network.outgoing.protocol]:
                  config.network.outgoing.filter,
              }
            : config.network.outgoing.filter,
      },
    };
  }
  
  if (config.fileSystem.enabled) {
    configObj.feature.fs = {
      mode: config.fileSystem.mode,
    };
  }
  
  if (config.environment.enabled) {
    configObj.feature.env = {};
    if (config.environment.include)
      configObj.feature.env.include = config.environment.include;
    if (config.environment.exclude)
      configObj.feature.env.exclude = config.environment.exclude;
    if (config.environment.override)
      configObj.feature.env.override = config.environment.override;
  }
  
  return JSON.stringify(configObj, null, 2);
};

export const validateJson = (jsonString: string, setJsonError: (error: string) => void): boolean => {
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

export const updateConfigFromJson = (
  jsonString: string,
  setConfig: React.Dispatch<React.SetStateAction<ConfigData>>,
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
