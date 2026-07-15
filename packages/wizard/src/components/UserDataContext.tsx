import React, { useContext, useState } from 'react'
import type { FeatureFileConfig, LayerFileConfig } from '../mirrord-schema'

export const UserDataContext = React.createContext<boolean | undefined>(
  undefined,
)

export interface ConfigData {
  config: LayerFileConfig
  setConfig: (config: LayerFileConfig) => void
}

export const DEFAULT_FEATURE: FeatureFileConfig = {
  network: {
    incoming: {
      mode: 'mirror',
    },
    outgoing: true,
  },
  fs: 'read',
  env: true,
}

export const DefaultConfig: LayerFileConfig = {
  feature: DEFAULT_FEATURE,
}

export const ConfigDataContextProvider = ({
  children,
}: {
  children: React.ReactNode
}) => {
  const [config, setConfig] = useState<LayerFileConfig>(DefaultConfig)
  return (
    <ConfigDataContext.Provider value={{ config, setConfig }}>
      {children}
    </ConfigDataContext.Provider>
  )
}

export const ConfigDataContext = React.createContext<ConfigData | undefined>(
  undefined,
)

export const useConfigData = (): ConfigData => {
  const context = useContext(ConfigDataContext)
  if (!context) {
    throw new Error(
      'useConfigData must be used within a ConfigDataContextProvider',
    )
  }
  return context
}
