import { LayerFileConfig } from "@/mirrord-schema";
import React, { useState } from "react";

export const UserDataContext = React.createContext<boolean | undefined>(
  undefined
);

export interface ConfigData {
  config: LayerFileConfig;
  setConfig: (config: LayerFileConfig) => void;
}

export const DefaultConfig: LayerFileConfig = {
  feature: {
    network: {
      incoming: {
        mode: "mirror",
        http_filter: {},
      },
      outgoing: true,
    },
    fs: "read",
    env: true,
  },
};

export const ConfigDataContextProvider = ({
  children,
}: {
  children: React.ReactNode;
}) => {
  const [config, setConfig] = useState<LayerFileConfig>(DefaultConfig);
  const setConfigWithLog = (config: LayerFileConfig) => {
    console.log("Setting config:", config);
    setConfig(config);
  };
  return (
    <ConfigDataContext.Provider value={{ config, setConfig: setConfigWithLog }}>
      {children}
    </ConfigDataContext.Provider>
  );
};

export const ConfigDataContext = React.createContext<ConfigData | undefined>(
  undefined
);
