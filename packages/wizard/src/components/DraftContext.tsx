import React, { useContext, useMemo, useState } from 'react'
import { newDraft, type PickedService, type ServiceDraft } from '../lib/up'

/** The finished services and the one being edited. */
export interface Draft {
  services: PickedService[]
  setServices: (services: PickedService[]) => void
  draft: ServiceDraft
  setDraft: (draft: ServiceDraft) => void
  /** Starts over. */
  reset: () => void
}

export const DraftContext = React.createContext<Draft | undefined>(undefined)

export const DraftContextProvider = ({
  children,
}: {
  children: React.ReactNode
}) => {
  const [services, setServices] = useState<PickedService[]>([])
  const [draft, setDraft] = useState<ServiceDraft>(newDraft())
  const value = useMemo(
    () => ({
      services,
      setServices,
      draft,
      setDraft,
      reset: () => {
        setServices([])
        setDraft(newDraft())
      },
    }),
    [services, draft],
  )
  return <DraftContext.Provider value={value}>{children}</DraftContext.Provider>
}

export const useDraft = (): Draft => {
  const context = useContext(DraftContext)
  if (!context) {
    throw new Error('useDraft must be used within a DraftContextProvider')
  }
  return context
}
