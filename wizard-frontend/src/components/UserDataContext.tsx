import React, { useState } from "react";

export const UserDataContext = React.createContext<boolean | undefined>(
  undefined
);

interface ConfigData {
  config: any;
  setConfig: (config: any) => void;
}

export const DefaultConfig = {
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
  const [config, setConfig] = useState<any>(DefaultConfig);
  const setConfigWithLog = (config: any) => {
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
